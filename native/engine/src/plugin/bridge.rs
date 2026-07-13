//! `EngineBridge` —— [`PluginBridge`] 实现：网络出口守卫 + storage + log + 重试意图。
//!
//! ## 网络出口守卫（防 SSRF）
//! 保护本机与 headless server 不被恶意/有 bug 的第三方插件访问内部服务。核心是
//! **单一判定函数** [`is_globally_routable_unicast`]，供三处复用（杜绝判定漂移）：
//! 1. 字面量 IP 前置校验（进入 reqwest 之前，挡 hyper-util 对字面量 IP 的短路）；
//! 2. 自定义 `reqwest::dns::Resolve`（解析后过滤，挡 DNS rebinding、消 TOCTOU）；
//! 3. 逐跳重定向 `Policy::custom`（手动重建 30 跳上限 + 每跳字面量 IP 校验）。
//!
//! ## v1 限制（记录在案）
//! - `proxy` 在 bridge 构造时快照（reqwest ClientBuilder 配置构建时定死）；运行期改
//!   代理后插件出口不随动（可接受，非安全问题）。
//! - 单次调用严格 per-call fetch 上限退化为**全局并发 fetch 上限**（对宿主保护更强）。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use tokio::sync::{Semaphore, mpsc};

use crate::db::Db;
use crate::logger::{log_error, log_info};
use crate::proxy_config::ProxyConfig;

use super::runtime::{
    BridgeHttpRequest, BridgeHttpResponse, PluginBridge, PluginError, PluginLogLevel,
};

/// 响应体上限（超限截断 + `truncated:true`）。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// 单请求超时。
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// 全局并发 fetch 上限。
const MAX_CONCURRENT_FETCH: usize = 8;
/// 重定向跳数上限（Policy::custom 丢失 limited 默认保护，手动重建）。
const MAX_REDIRECTS: usize = 30;
/// 单值上限 64KB。
const MAX_STORAGE_VALUE: usize = 64 * 1024;
/// 单插件 storage 键数上限。
const MAX_STORAGE_KEYS: usize = 100;
/// 单条日志截断长度。
const MAX_LOG_BYTES: usize = 4 * 1024;

/// 唯一的「可全局路由单播」判定函数。三处复用，杜绝判定逻辑漂移。
///
/// 拒绝一切环回/私网/链路本地/文档/保留/元数据段等非公网单播地址。
///
/// # Examples
///
/// ```
/// use std::net::IpAddr;
/// use fluxdown_engine::plugin::bridge::is_globally_routable_unicast;
///
/// assert!(is_globally_routable_unicast("8.8.8.8".parse::<IpAddr>().unwrap()));
/// assert!(!is_globally_routable_unicast("127.0.0.1".parse::<IpAddr>().unwrap()));
/// assert!(!is_globally_routable_unicast("169.254.169.254".parse::<IpAddr>().unwrap()));
/// ```
pub fn is_globally_routable_unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_routable(v4),
        IpAddr::V6(v6) => is_v6_routable(v6),
    }
}

fn is_v4_routable(ip: Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    let o = ip.octets();
    // 0.0.0.0/8 "this network"
    if o[0] == 0 {
        return false;
    }
    // 100.64.0.0/10 CGNAT
    if o[0] == 100 && (o[1] & 0xc0) == 0x40 {
        return false;
    }
    // 198.18.0.0/15 benchmarking
    if o[0] == 198 && (o[1] & 0xfe) == 18 {
        return false;
    }
    // 240.0.0.0/4 reserved
    if (o[0] & 0xf0) == 240 {
        return false;
    }
    true
}

fn is_v6_routable(ip: Ipv6Addr) -> bool {
    // IPv4-mapped ::ffff:a.b.c.d → 解包递归。
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_v4_routable(v4);
    }
    let seg = ip.segments();
    // 6to4 2002::/16 → 内嵌 IPv4 递归。
    if seg[0] == 0x2002 {
        let v4 = Ipv4Addr::new(
            (seg[1] >> 8) as u8,
            (seg[1] & 0xff) as u8,
            (seg[2] >> 8) as u8,
            (seg[2] & 0xff) as u8,
        );
        return is_v4_routable(v4);
    }
    // NAT64 64:ff9b::/96 → 末 32bit 递归。
    if seg[0] == 0x0064 && seg[1] == 0xff9b {
        let v4 = Ipv4Addr::new(
            (seg[6] >> 8) as u8,
            (seg[6] & 0xff) as u8,
            (seg[7] >> 8) as u8,
            (seg[7] & 0xff) as u8,
        );
        return is_v4_routable(v4);
    }
    if ip.is_loopback()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
    {
        return false;
    }
    // Teredo 2001:0000::/32
    if seg[0] == 0x2001 && seg[1] == 0x0000 {
        return false;
    }
    // documentation 2001:0db8::/32
    if seg[0] == 0x2001 && seg[1] == 0x0db8 {
        return false;
    }
    true
}

/// 守卫用自定义 DNS 解析器：解析后仅保留可全局路由的地址。
struct GuardResolver;

impl reqwest::dns::Resolve for GuardResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let filtered: Vec<SocketAddr> =
                addrs.filter(|sa| is_globally_routable_unicast(sa.ip())).collect();
            let iter: reqwest::dns::Addrs = Box::new(filtered.into_iter());
            Ok(iter)
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum GuardError {
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("blocked: non-routable redirect target")]
    BlockedRedirect,
}

/// 引擎侧 `PluginBridge` 实现。
pub struct EngineBridge {
    client: reqwest::Client,
    db: Db,
    plugin_retry_tx: mpsc::UnboundedSender<(String, u64)>,
    fetch_sema: Arc<Semaphore>,
}

impl EngineBridge {
    /// 构造带守卫的 bridge。`proxy` 在此快照进 Client（v1 限制，见模块文档）。
    pub fn new(
        db: Db,
        proxy: &ProxyConfig,
        plugin_retry_tx: mpsc::UnboundedSender<(String, u64)>,
    ) -> Result<Self, PluginError> {
        let mut builder = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .dns_resolver(Arc::new(GuardResolver))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error(GuardError::TooManyRedirects);
                }
                if let Some(host) = attempt.url().host_str() {
                    let trimmed = host.trim_matches(|c| c == '[' || c == ']');
                    if let Ok(ip) = trimmed.parse::<IpAddr>()
                        && !is_globally_routable_unicast(ip)
                    {
                        return attempt.error(GuardError::BlockedRedirect);
                    }
                }
                attempt.follow()
            }));

        if let Some(url) = proxy.resolve().to_proxy_url()
            && let Ok(p) = reqwest::Proxy::all(&url)
        {
            builder = builder.proxy(p);
        }

        let client = builder
            .build()
            .map_err(|e| PluginError::Runtime(format!("构建守卫 Client 失败: {e}")))?;
        Ok(Self {
            client,
            db,
            plugin_retry_tx,
            fetch_sema: Arc::new(Semaphore::new(MAX_CONCURRENT_FETCH)),
        })
    }
}

#[async_trait::async_trait]
impl PluginBridge for EngineBridge {
    async fn http_request(
        &self,
        _plugin_id: &str,
        req: BridgeHttpRequest,
    ) -> Result<BridgeHttpResponse, PluginError> {
        // scheme 仅 http/https。
        let parsed = url::Url::parse(&req.url)
            .map_err(|e| PluginError::InvalidOutput(format!("URL 非法: {e}")))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(PluginError::InvalidOutput(
                "flux.fetch 仅支持 http/https".to_string(),
            ));
        }
        // 字面量 IP 前置校验（挡 hyper-util 对字面量 IP 的短路）。
        if let Some(host) = parsed.host_str() {
            let trimmed = host.trim_matches(|c| c == '[' || c == ']');
            if let Ok(ip) = trimmed.parse::<IpAddr>()
                && !is_globally_routable_unicast(ip)
            {
                return Err(PluginError::InvalidOutput(
                    "blocked: non-routable IP".to_string(),
                ));
            }
        }

        let permit = self
            .fetch_sema
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PluginError::Runtime("fetch semaphore closed".to_string()))?;
        let _permit = permit;

        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| PluginError::InvalidOutput(format!("HTTP method 非法: {}", req.method)))?;
        let mut rb = self.client.request(method, parsed);
        for (k, v) in &req.headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                rb = rb.header(name, value);
            }
        }
        if let Some(body) = req.body {
            rb = rb.body(body);
        }

        let mut resp = rb
            .send()
            .await
            .map_err(|e| PluginError::Runtime(format!("fetch 失败: {e}")))?;
        let status = resp.status().as_u16();
        let mut headers = std::collections::HashMap::new();
        for (k, v) in resp.headers() {
            if let Ok(s) = v.to_str() {
                headers.insert(k.as_str().to_string(), s.to_string());
            }
        }

        let mut body = Vec::new();
        let mut truncated = false;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY_BYTES {
                        let take = MAX_BODY_BYTES - body.len();
                        body.extend_from_slice(&chunk[..take]);
                        truncated = true;
                        break;
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => return Err(PluginError::Runtime(format!("读取响应体失败: {e}"))),
            }
        }

        Ok(BridgeHttpResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).to_string(),
            truncated,
        })
    }

    async fn storage_get(&self, plugin_id: &str, key: &str) -> Option<String> {
        let full = format!("plugin.{plugin_id}.kv.{key}");
        self.db.get_config(&full).await.ok().flatten()
    }

    async fn storage_set(
        &self,
        plugin_id: &str,
        key: &str,
        value: String,
    ) -> Result<(), PluginError> {
        if value.len() > MAX_STORAGE_VALUE {
            return Err(PluginError::InvalidOutput(format!(
                "storage 值超过 {MAX_STORAGE_VALUE} 字节上限"
            )));
        }
        let prefix = format!("plugin.{plugin_id}.kv.");
        let full = format!("{prefix}{key}");
        // 键数上限：仅当是新键时才计数。
        if let Ok(existing) = self.db.list_config_with_prefix(&prefix).await {
            let is_new = !existing.iter().any(|(k, _)| k == &full);
            if is_new && existing.len() >= MAX_STORAGE_KEYS {
                return Err(PluginError::InvalidOutput(format!(
                    "storage 键数超过 {MAX_STORAGE_KEYS} 上限"
                )));
            }
        }
        self.db
            .set_config(&full, &value)
            .await
            .map_err(|e| PluginError::Runtime(format!("storage 写入失败: {e}")))
    }

    fn log(&self, plugin_id: &str, level: PluginLogLevel, message: &str) {
        let truncated = if message.len() > MAX_LOG_BYTES {
            // 按字符边界安全截断。
            let mut end = MAX_LOG_BYTES;
            while end > 0 && !message.is_char_boundary(end) {
                end -= 1;
            }
            &message[..end]
        } else {
            message
        };
        match level {
            PluginLogLevel::Error => log_error!("[plugin:{}] {}", plugin_id, truncated),
            _ => log_info!("[plugin:{}] {}", plugin_id, truncated),
        }
    }

    fn request_retry(&self, task_id: &str, delay_ms: u64) {
        // fire-and-forget；限流在 actor 侧（max_auto_retries）。
        let _ = self
            .plugin_retry_tx
            .send((task_id.to_string(), delay_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::is_globally_routable_unicast;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap_or_else(|_| panic!("bad ip {s}"))
    }

    #[test]
    fn v4_rejects_private_and_special() {
        for s in [
            "127.0.0.1",       // loopback
            "10.0.0.1",        // private
            "172.16.0.1",      // private
            "192.168.1.1",     // private
            "169.254.169.254", // link-local (cloud metadata)
            "100.64.0.1",      // CGNAT
            "198.18.0.1",      // benchmarking
            "240.0.0.1",       // reserved
            "0.0.0.0",         // this-network
            "255.255.255.255", // broadcast
            "224.0.0.1",       // multicast
            "192.0.2.1",       // documentation
        ] {
            assert!(!is_globally_routable_unicast(ip(s)), "{s} should be rejected");
        }
    }

    #[test]
    fn v4_allows_public() {
        for s in ["8.8.8.8", "1.1.1.1", "93.184.216.34"] {
            assert!(is_globally_routable_unicast(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn v6_rejects_special() {
        for s in [
            "::1",                    // loopback
            "fe80::1",                // link-local
            "fc00::1",                // ULA
            "fd00::1",                // ULA
            "2001:db8::1",            // documentation
            "2001:0::1",             // Teredo
            "ff02::1",                // multicast
            "::",                     // unspecified
            "::ffff:127.0.0.1",       // v4-mapped loopback
            "2002:0a00:0001::",       // 6to4 embedding 10.0.0.1
            "64:ff9b::0a00:0001",     // NAT64 embedding 10.0.0.1
        ] {
            assert!(!is_globally_routable_unicast(ip(s)), "{s} should be rejected");
        }
    }

    #[test]
    fn v6_allows_public() {
        for s in ["2606:4700:4700::1111", "2001:4860:4860::8888"] {
            assert!(is_globally_routable_unicast(ip(s)), "{s} should be allowed");
        }
    }
}
