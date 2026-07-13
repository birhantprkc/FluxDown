//! v1 唯一 `ScriptRuntime` 实现 —— `QuickJsScriptRuntime`。**rquickjs 类型只存在于
//! 本文件**（抽象层 [`super::runtime`] 禁止 rquickjs 类型，未来可换 deno_core）。
//!
//! 设计要点（spike `examples/plugin_spike.rs` 已在 Windows MSVC 实测坐实）：
//! - 专用 `tokio::runtime::Runtime`（`min(4,cpu)` 线程，仿 BT 先例），与主 actor
//!   （current_thread）、BT runtime 物理隔离。
//! - 两个独立信号量（resolve/hook 拆池，两平面正交）。
//! - **每次调用全新 `AsyncRuntime`+`AsyncContext`**（QuickJS 初始化亚毫秒级，无跨调用
//!   状态泄漏；全局锁问题因不共享 Runtime 而消失）。
//! - `set_memory_limit` + `set_interrupt_handler`（deadline 闭包，最佳努力）+ 外层
//!   `tokio::time::timeout`（不依赖 QuickJS 检查点，覆盖 await 挂起）。
//! - 脚本以 **classic script** 加载（`Context::eval`，非 ESM），入口函数挂 `globalThis`。

use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, Context, Ctx, Function, Promise, Runtime,
    prelude::Async,
};
use tokio::sync::Semaphore;

use super::runtime::{
    ExecutionBudget, PluginBridge, PluginEntryKind, PluginError, PluginEvent, PluginLogLevel,
    PluginScript, ResolveRequest, ResolveResult, ScriptRuntime,
};

/// 硬顶：任何单次调用（含 manifest timeoutMs）都不得超过 30s。
pub const HARD_TIMEOUT_CEILING: Duration = Duration::from_secs(30);
/// resolve 信号量 acquire 超时（超时 → `Overloaded`，fail-closed）。
const RESOLVE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(3);
/// resolve 返回 null/undefined 的哨兵。
const NULL_SENTINEL: &str = "__FLUX_NULL__";
/// storage.get 无值的哨兵。
const STORAGE_NONE: &str = "__FLUX_NONE__";
/// 同步 eval（check_compile / regex）的内存上限——挡恶意 manifest/pattern 在 actor
/// 线程上 OOM/ReDoS。
const SYNC_EVAL_MEMORY: usize = 32 * 1024 * 1024;
/// 同步 eval 的中断截止（interrupt handler 按字节码计数）——挡 while(true) 死循环 /
/// 灾难性回溯 RegExp 冻结 actor。
const SYNC_EVAL_TIMEOUT: Duration = Duration::from_secs(2);

/// v1 QuickJS 运行时实现。
pub struct QuickJsScriptRuntime {
    /// 专用 multi_thread runtime。`Option` 以便在 [`Drop`] 中 `shutdown_background`
    /// 取出——直接 drop 一个 multi_thread Runtime 若发生在异步上下文中会 panic
    /// （"Cannot drop a runtime in a context where blocking is not allowed"）。
    runtime: Option<tokio::runtime::Runtime>,
    /// runtime 的 handle（cheap clone，供 `spawn_handle` 恒可用，与 runtime 生命周期同步）。
    handle: tokio::runtime::Handle,
    /// resolve 平面信号量：固定容量 `max(启动时 max_concurrent, workers)`。
    resolve_sema: Arc<Semaphore>,
    /// hook 平面信号量：容量 = workers；`try_acquire` 失败即丢。
    hook_sema: Arc<Semaphore>,
}

impl Drop for QuickJsScriptRuntime {
    fn drop(&mut self) {
        // 非阻塞关停：不等待 in-flight 脚本（进程/引擎退出时 pending resolve 放弃即可），
        // 避免在异步上下文中 drop multi_thread Runtime 触发 panic。
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

impl QuickJsScriptRuntime {
    /// 构造专用运行时。`max_concurrent_at_startup` 用于 resolve 信号量容量下界。
    pub fn new(max_concurrent_at_startup: usize) -> std::io::Result<Self> {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let workers = std::cmp::min(4, cpus.max(1));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("plugin-runtime")
            .enable_all()
            .build()?;
        let handle = runtime.handle().clone();
        let resolve_cap = max_concurrent_at_startup.max(workers).max(1);
        Ok(Self {
            runtime: Some(runtime),
            handle,
            resolve_sema: Arc::new(Semaphore::new(resolve_cap)),
            hook_sema: Arc::new(Semaphore::new(workers.max(1))),
        })
    }

    /// clamp budget.timeout 到硬顶。
    fn clamp(budget: ExecutionBudget) -> ExecutionBudget {
        ExecutionBudget {
            timeout: budget.timeout.min(HARD_TIMEOUT_CEILING),
            memory_limit_bytes: budget.memory_limit_bytes,
        }
    }

    /// 同步跑一段返回 boolean 的 JS（带内存/中断预算，任何错误 → false）。
    fn eval_bool(src: &str) -> bool {
        let Ok(rt) = Runtime::new() else {
            return false;
        };
        rt.set_memory_limit(SYNC_EVAL_MEMORY);
        let deadline = Instant::now() + SYNC_EVAL_TIMEOUT;
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let Ok(ctx) = Context::full(&rt) else {
            return false;
        };
        ctx.with(|ctx| ctx.eval::<bool, _>(src).catch(&ctx).unwrap_or(false))
    }

    /// 执行一段脚本并调用入口函数，返回 JS 侧字符串结果（resolve 用；hook 返回空串）。
    ///
    /// `entry` = 入口全局函数名；`arg_json` = 传给入口的参数 JSON；`retry_task_id`
    /// = Some(task_id) 时 `flux.task.requestRetry` 生效（onError 专用），否则 warn 忽略。
    #[allow(clippy::too_many_arguments)]
    async fn run_script(
        &self,
        source: String,
        entry: &'static str,
        is_resolve: bool,
        arg_json: String,
        settings_json: String,
        info_json: String,
        retry_task_id: Option<String>,
        bridge: Arc<dyn PluginBridge>,
        plugin_id: String,
        budget: ExecutionBudget,
    ) -> Result<String, PluginError> {
        let budget = Self::clamp(budget);

        let rt = AsyncRuntime::new().map_err(|e| PluginError::Runtime(e.to_string()))?;
        rt.set_memory_limit(budget.memory_limit_bytes).await;
        let deadline = Instant::now() + budget.timeout;
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)))
            .await;
        let ctx = AsyncContext::full(&rt)
            .await
            .map_err(|e| PluginError::Runtime(e.to_string()))?;

        let entry_owned = entry.to_string();
        let exec = ctx.async_with(async move |ctx| -> Result<String, PluginError> {
            inject_bridge(&ctx, &bridge, &plugin_id, retry_task_id)
                .map_err(|e| PluginError::Runtime(format!("注入 flux 失败: {e}")))?;

            // 注入类型化设置 / info / 入口参数。
            set_json_global(&ctx, "__flux_settings_json", &settings_json)?;
            set_json_global(&ctx, "__flux_info", &info_json)?;
            set_json_global(&ctx, "__flux_arg", &arg_json)?;

            // 先建 flux/console 门面，再 eval 插件源码（定义 globalThis 入口）。
            ctx.eval::<rquickjs::Value, _>(FLUX_PRELUDE)
                .catch(&ctx)
                .map_err(|e| PluginError::Runtime(format!("flux 门面初始化失败: {e}")))?;
            ctx.eval::<rquickjs::Value, _>(source.as_bytes())
                .catch(&ctx)
                .map_err(|e| PluginError::CompileFailed(e.to_string()))?;

            // 调入口 → Promise<String>。
            let wrapper = build_entry_wrapper(&entry_owned, is_resolve);
            let promise: Promise = ctx
                .eval(wrapper.as_bytes())
                .catch(&ctx)
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
            let out: String = promise
                .into_future()
                .await
                .map_err(|e| PluginError::Runtime(e.to_string()))?;
            Ok(out)
        });

        match tokio::time::timeout(budget.timeout, exec).await {
            Ok(r) => {
                // 尽力回收 job 队列（不阻塞主流程）。
                rt.idle().await;
                r
            }
            Err(_) => Err(PluginError::Timeout),
        }
    }
}

#[async_trait::async_trait]
impl ScriptRuntime for QuickJsScriptRuntime {
    fn check_compile(&self, source: &str) -> Result<(), PluginError> {
        let rt = Runtime::new().map_err(|e| PluginError::CompileFailed(e.to_string()))?;
        // 预算：classic eval 会执行顶层代码，须挡恶意插件在 actor 线程上 OOM/死循环。
        rt.set_memory_limit(SYNC_EVAL_MEMORY);
        let deadline = Instant::now() + SYNC_EVAL_TIMEOUT;
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
        let ctx = Context::full(&rt).map_err(|e| PluginError::CompileFailed(e.to_string()))?;
        ctx.with(|ctx| {
            ctx.eval::<rquickjs::Value, _>(source)
                .catch(&ctx)
                .map(|_| ())
                .map_err(|e| PluginError::CompileFailed(e.to_string()))
        })
    }

    fn regex_valid(&self, pattern: &str) -> bool {
        let lit = serde_json::to_string(pattern).unwrap_or_else(|_| "\"\"".to_string());
        Self::eval_bool(&format!(
            "(function(){{ try {{ new RegExp({lit}); return true; }} catch(e) {{ return false; }} }})()"
        ))
    }

    fn regex_test(&self, pattern: &str, value: &str) -> bool {
        let plit = serde_json::to_string(pattern).unwrap_or_else(|_| "\"\"".to_string());
        let vlit = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string());
        Self::eval_bool(&format!(
            "(function(){{ try {{ return new RegExp({plit}).test({vlit}); }} catch(e) {{ return false; }} }})()"
        ))
    }

    async fn invoke_resolve(
        &self,
        plugin: &PluginScript,
        req: ResolveRequest,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
    ) -> Result<Option<ResolveResult>, PluginError> {
        // resolve 平面：acquire 3s 超时 → Overloaded（fail-closed）。
        let permit = tokio::time::timeout(
            RESOLVE_ACQUIRE_TIMEOUT,
            self.resolve_sema.clone().acquire_owned(),
        )
        .await
        .map_err(|_| PluginError::Overloaded)?
        .map_err(|_| PluginError::Overloaded)?;
        let _permit = permit;

        let arg_json =
            serde_json::to_string(&req).map_err(|e| PluginError::Runtime(e.to_string()))?;
        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);

        let raw = self
            .run_script(
                plugin.source.clone(),
                "resolve",
                true,
                arg_json,
                settings_json,
                info_json,
                None,
                bridge,
                plugin.identity.clone(),
                budget,
            )
            .await?;

        if raw == NULL_SENTINEL {
            return Ok(None);
        }
        let result: ResolveResult = serde_json::from_str(&raw)
            .map_err(|e| PluginError::InvalidOutput(format!("resolve 返回值非法: {e}")))?;
        Ok(Some(result))
    }

    async fn invoke_hook(
        &self,
        plugin: &PluginScript,
        event: PluginEvent,
        settings_json: String,
        bridge: Arc<dyn PluginBridge>,
        budget: ExecutionBudget,
    ) {
        debug_assert_eq!(plugin.entry_fn_hint, PluginEntryKind::Hook);
        // hook 平面：try_acquire，失败即静默丢弃（不等待、不影响任何计数）。
        let Ok(permit) = self.hook_sema.clone().try_acquire_owned() else {
            return;
        };
        let _permit = permit;

        let arg_json = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(_) => return,
        };
        let entry = event.hook_fn_name();
        let retry_task_id = match &event {
            PluginEvent::Error { task_id, .. } => Some(task_id.clone()),
            _ => None,
        };
        let info_json = info_json(&plugin.identity, &plugin.version, &plugin.app_version);
        let identity = plugin.identity.clone();
        let log_bridge = bridge.clone();

        if let Err(e) = self
            .run_script(
                plugin.source.clone(),
                entry,
                false,
                arg_json,
                settings_json,
                info_json,
                retry_task_id,
                bridge,
                identity.clone(),
                budget,
            )
            .await
        {
            // fire-and-forget：仅记日志。
            log_bridge.log(
                &identity,
                PluginLogLevel::Warn,
                &format!("hook {entry} 执行失败（已忽略）: {e}"),
            );
        }
    }

    fn spawn_handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
}

/// 构建 `flux.info` 的 JSON。
fn info_json(identity: &str, version: &str, app_version: &str) -> String {
    serde_json::json!({
        "identity": identity,
        "version": version,
        "appVersion": app_version,
    })
    .to_string()
}

/// 把 JSON 字符串 parse 为 JS 值并挂到全局。
fn set_json_global(ctx: &Ctx<'_>, name: &str, json: &str) -> Result<(), PluginError> {
    let v = ctx
        .json_parse(json.as_bytes().to_vec())
        .catch(ctx)
        .map_err(|e| PluginError::Runtime(format!("解析 {name} 失败: {e}")))?;
    ctx.globals()
        .set(name, v)
        .map_err(|e| PluginError::Runtime(format!("设置 {name} 失败: {e}")))
}

/// 构建调用入口函数并 stringify 结果的 wrapper。
fn build_entry_wrapper(entry: &str, is_resolve: bool) -> String {
    let entry_lit = serde_json::to_string(entry).unwrap_or_else(|_| "\"resolve\"".to_string());
    if is_resolve {
        format!(
            "(async () => {{ const __fn = globalThis[{entry_lit}]; \
             if (typeof __fn !== 'function') throw new Error('入口 '+{entry_lit}+' 未定义'); \
             const __r = await __fn(__flux_arg); \
             return (__r === null || __r === undefined) ? '{NULL_SENTINEL}' : JSON.stringify(__r); }})()"
        )
    } else {
        format!(
            "(async () => {{ const __fn = globalThis[{entry_lit}]; \
             if (typeof __fn === 'function') {{ await __fn(__flux_arg); }} \
             return ''; }})()"
        )
    }
}

/// 注入低层 `__flux_*` 桥接函数（异步 fetch/storage、同步 log/requestRetry）。
fn inject_bridge(
    ctx: &Ctx<'_>,
    bridge: &Arc<dyn PluginBridge>,
    plugin_id: &str,
    retry_task_id: Option<String>,
) -> Result<(), rquickjs::Error> {
    let globals = ctx.globals();

    // __flux_fetch(optsJson) -> Promise<String(JSON)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |opts: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let req: super::runtime::BridgeHttpRequest =
                        serde_json::from_str(&opts).unwrap_or_default();
                    let payload = match b.http_request(&pid, req).await {
                        Ok(resp) => serde_json::json!({
                            "value": {
                                "status": resp.status,
                                "headers": resp.headers,
                                "body": resp.body,
                                "truncated": resp.truncated,
                            }
                        }),
                        Err(e) => serde_json::json!({ "__fluxError": e.to_string() }),
                    };
                    Ok::<String, rquickjs::Error>(payload.to_string())
                }
            }),
        )?
        .with_name("__flux_fetch")?;
        globals.set("__flux_fetch", f)?;
    }

    // __flux_storage_get(key) -> Promise<String>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |key: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let v = b
                        .storage_get(&pid, &key)
                        .await
                        .unwrap_or_else(|| STORAGE_NONE.to_string());
                    Ok::<String, rquickjs::Error>(v)
                }
            }),
        )?
        .with_name("__flux_storage_get")?;
        globals.set("__flux_storage_get", f)?;
    }

    // __flux_storage_set(key, value) -> Promise<String("" 或错误消息)>
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            Async(move |key: String, value: String| {
                let b = b.clone();
                let pid = pid.clone();
                async move {
                    let msg = match b.storage_set(&pid, &key, value).await {
                        Ok(()) => String::new(),
                        Err(e) => e.to_string(),
                    };
                    Ok::<String, rquickjs::Error>(msg)
                }
            }),
        )?
        .with_name("__flux_storage_set")?;
        globals.set("__flux_storage_set", f)?;
    }

    // __flux_log(level, msg) -> ()（同步）
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            move |level: String, msg: String| {
                let lvl = match level.as_str() {
                    "warn" => PluginLogLevel::Warn,
                    "error" => PluginLogLevel::Error,
                    _ => PluginLogLevel::Info,
                };
                b.log(&pid, lvl, &msg);
            },
        )?
        .with_name("__flux_log")?;
        globals.set("__flux_log", f)?;
    }

    // __flux_request_retry(delayStr) -> ()（同步；仅 onError 生效）
    {
        let b = bridge.clone();
        let pid = plugin_id.to_string();
        let f = Function::new(
            ctx.clone(),
            move |delay: String| {
                let delay_ms = delay.parse::<u64>().unwrap_or(0);
                match &retry_task_id {
                    Some(tid) => b.request_retry(tid, delay_ms),
                    None => b.log(
                        &pid,
                        PluginLogLevel::Warn,
                        "flux.task.requestRetry 仅 onError 钩子可用，已忽略",
                    ),
                }
            },
        )?
        .with_name("__flux_request_retry")?;
        globals.set("__flux_request_retry", f)?;
    }

    Ok(())
}

/// flux/console 门面（引用上面注入的 `__flux_*` 与 `__flux_settings_json`/`__flux_info`）。
const FLUX_PRELUDE: &str = r#"
(function () {
  const __args2str = (a) => a.map(x => {
    try { return typeof x === 'string' ? x : JSON.stringify(x); } catch (e) { return String(x); }
  }).join(' ');
  globalThis.flux = {
    fetch: (opts) => __flux_fetch(JSON.stringify(opts || {})).then((s) => {
      const r = JSON.parse(s);
      if (r.__fluxError) throw new Error(r.__fluxError);
      return r.value;
    }),
    storage: {
      get: (key) => __flux_storage_get(String(key)).then((s) => s === '__FLUX_NONE__' ? null : s),
      set: (key, value) => __flux_storage_set(String(key), String(value)).then((s) => {
        if (s) throw new Error(s);
      }),
    },
    settings: __flux_settings_json,
    info: __flux_info,
    logger: {
      info: (...a) => __flux_log('info', __args2str(a)),
      warn: (...a) => __flux_log('warn', __args2str(a)),
      error: (...a) => __flux_log('error', __args2str(a)),
    },
    task: {
      requestRetry: (opts) => __flux_request_retry(String((opts && opts.delayMs) || 0)),
    },
  };
  globalThis.console = {
    log: (...a) => __flux_log('info', __args2str(a)),
    info: (...a) => __flux_log('info', __args2str(a)),
    warn: (...a) => __flux_log('warn', __args2str(a)),
    error: (...a) => __flux_log('error', __args2str(a)),
    debug: (...a) => __flux_log('info', __args2str(a)),
  };
})();
"#;
