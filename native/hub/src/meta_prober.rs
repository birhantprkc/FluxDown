//! 队列任务元数据探测 — 在任务等待期间后台探测文件名和大小。
//!
//! 支持协议:
//! - HTTP/HTTPS → HEAD 请求获取 Content-Disposition / Content-Length
//! - FTP        → 复用 ftp_downloader::resolve_ftp_file_info（SIZE 命令）
//! - magnet:    → 提取 dn= 参数作为文件名（无大小信息）
//! - torrent-file:// → 跳过（名称由 librqbit 解析后上报）

use tokio::time::Duration;

use crate::downloader::extract_filename;

/// 探测超时（秒）
const PROBE_TIMEOUT_SECS: u64 = 8;

/// 探测队列任务的文件名和大小。
///
/// 返回 `(file_name, total_bytes)`。
/// - `file_name` 为空表示无法探测或已有名称（file_name 参数非空时跳过名称探测）
/// - `total_bytes` 为 0 表示未知大小
pub async fn probe_task_meta(
    url: &str,
    file_name: &str, // DB 中已有的文件名；非空则跳过名称探测
    client: &reqwest::Client,
    proxy_config: &crate::proxy_config::ProxyConfig,
) -> (String, i64) {
    // torrent-file:// 任务的名称由 librqbit 元数据解析后上报，跳过探测
    if url.starts_with("torrent-file://") {
        return (String::new(), 0);
    }

    // 仅取前 8 字节做协议判断，避免不必要的堆分配
    let lower_prefix = url
        .get(..8)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    // magnet: — 从 dn= 参数提取文件名，无大小
    if lower_prefix.starts_with("magnet:") {
        let name = if file_name.is_empty() {
            extract_dn_from_magnet(url)
        } else {
            String::new()
        };
        return (name, 0);
    }

    // ftp:// — 使用现有 FTP 解析逻辑
    if lower_prefix.starts_with("ftp://") {
        return probe_ftp_meta(url, proxy_config).await;
    }

    // HTTP / HTTPS
    probe_http_meta(url, file_name, client).await
}

// ---------------------------------------------------------------------------
// magnet dn= 提取
// ---------------------------------------------------------------------------

fn extract_dn_from_magnet(url: &str) -> String {
    // magnet:?xt=urn:btih:HASH&dn=NAME&tr=...
    let query = url.split_once('?').map(|x| x.1).unwrap_or("");
    for part in query.split('&') {
        if let Some(val) = part.strip_prefix("dn=") {
            let decoded = url_decode(val);
            if !decoded.is_empty() {
                return crate::downloader::sanitize_filename(&decoded);
            }
        }
    }
    String::new()
}

/// 简易 URL 解码（%XX 转义 + + → 空格）
fn url_decode(s: &str) -> String {
    let mut result = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            result.push(b' ');
            i += 1;
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                result.push(byte);
                i += 3;
                continue;
            }
            result.push(bytes[i]);
            i += 1;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

// ---------------------------------------------------------------------------
// FTP 探测（复用 ftp_downloader 的解析逻辑）
// ---------------------------------------------------------------------------

async fn probe_ftp_meta(
    url: &str,
    proxy_config: &crate::proxy_config::ProxyConfig,
) -> (String, i64) {
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_SECS),
        crate::ftp_downloader::resolve_ftp_file_info(url, proxy_config),
    )
    .await;
    match result {
        Ok(Ok(info)) => (info.file_name, info.total_bytes),
        _ => (String::new(), 0),
    }
}

// ---------------------------------------------------------------------------
// HTTP / HTTPS 探测
// ---------------------------------------------------------------------------

async fn probe_http_meta(url: &str, file_name: &str, client: &reqwest::Client) -> (String, i64) {
    let result = tokio::time::timeout(
        Duration::from_secs(PROBE_TIMEOUT_SECS),
        client.head(url).send(),
    )
    .await;

    match result {
        Ok(Ok(response)) => {
            let name = if file_name.is_empty() {
                extract_filename(response.headers(), url)
            } else {
                String::new()
            };
            let size = response.content_length().map(|s| s as i64).unwrap_or(0);
            (name, size)
        }
        _ => (String::new(), 0),
    }
}
