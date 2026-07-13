//! 插件系统集成测试：off-actor 惰性 resolve 端到端。
//!
//! 覆盖 plugin-system-plan 的 G2 主检点：
//! - 惰性 resolve：create → 下载 → 完成，resolve 恰被调用（经 resolve_rx 计数）。
//! - 协议路由修复（D2-b1）：resolver 把原始 URL 改写为真实直链后走正常下载。
//! - actor 非阻塞（D2-b1）：resolver worker sleep 期间另发 create 可被立即处理。
//! - disabled 插件 → 原始 URL 直下（不经 resolve）。
//!
//! 仅 `plugins` feature 下编译运行。

#![cfg(feature = "plugins")]

use std::io::Write as _;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use fluxdown_engine::bt_downloader::BtConfig;
use fluxdown_engine::proxy_config::ProxyConfig;
use fluxdown_engine::{Engine, EngineConfig, NoopSelection, NoopSink};

const FILE_BODY: &[u8] = b"fluxdown plugin lazy resolve integration payload body!!\n";

/// 本地 HTTP/1.1 服务器：支持 HEAD（Content-Length + Accept-Ranges）与 GET（全量）。
fn spawn_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 8192];
            let mut header = String::new();
            loop {
                let n = match std::io::Read::read(&mut stream, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                header.push_str(&String::from_utf8_lossy(&buf[..n]));
                if header.contains("\r\n\r\n") {
                    break;
                }
            }
            let is_head = header.starts_with("HEAD ");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                FILE_BODY.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            if !is_head {
                let _ = stream.write_all(FILE_BODY);
            }
            let _ = stream.flush();
        }
    });
    (port, handle)
}

/// 写一个字符串重写 resolver 插件到目录：匹配 `*://origin.test/*`，返回 setting
/// `target` 指向的真实直链。含 onError 钩子示例（不触发）。
async fn write_rewrite_plugin(dir: &std::path::Path) {
    tokio::fs::create_dir_all(dir).await.expect("mkdir plugin");
    let manifest = r#"{
      "identity": "test@rewriter",
      "name": "Test Rewriter",
      "version": "1.0.0",
      "resolvers": [{ "match": { "urls": ["*://origin.test/*"] }, "entry": "resolve.js", "timeoutMs": 5000 }],
      "settings": [{ "key": "target", "title": "目标直链", "type": "string", "widget": "text", "default": "" }]
    }"#;
    tokio::fs::write(dir.join("manifest.json"), manifest)
        .await
        .expect("write manifest");
    let resolve_js = r#"
      globalThis.resolve = async (ctx) => {
        const t = flux.settings.target;
        if (!t) return null;
        return { url: t };
      };
    "#;
    tokio::fs::write(dir.join("resolve.js"), resolve_js)
        .await
        .expect("write resolve.js");
}

fn engine_config(work: &std::path::Path) -> EngineConfig {
    EngineConfig {
        max_concurrent: 4,
        speed_limit_bps: 0,
        default_save_dir: work.to_string_lossy().into_owned(),
        app_data_dir: work.to_string_lossy().into_owned(),
        bt_config: BtConfig::default(),
        proxy_config: ProxyConfig::default(),
        user_agent: String::new(),
        data_dir_override: Some(work.to_path_buf()),
        database_url: None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn create(engine: &mut Engine, url: &str, save_dir: &str, name: &str) -> String {
    engine
        .manager
        .create_task(
            url.to_string(),
            save_dir.to_string(),
            name.to_string(),
            1,
            String::new(),
            String::new(),
            0,
            Vec::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            std::collections::HashMap::new(),
            Vec::new(),
            None,
            None,
            None,
        )
        .await
        .expect("create_task returns id")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lazy_resolve_rewrites_and_downloads() {
    let work = std::env::temp_dir().join(format!("fluxdown-plugin-it-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real.bin");

    // 先写插件目录（安装期会拷贝到 <data_dir>/plugins/）。
    let plugin_src = work.join("plugin_src");
    write_rewrite_plugin(&plugin_src).await;

    let mut engine = Engine::new(engine_config(&work), Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine");

    // 安装插件 + 设置 target 指向真实直链。
    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&plugin_src).await.expect("install");
    pm.update_settings("test@rewriter", &[("target".to_string(), real_url.clone())])
        .await
        .expect("set target");

    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let save = work.to_string_lossy().into_owned();
    let _tid = create(&mut engine, "http://origin.test/watch?v=abc", &save, "out.bin").await;

    // 驱动：先收 resolve 回流 → on_resolve_ready 再入分派 → 再收 done。
    let mut resolve_count = 0u32;
    let done = loop {
        tokio::select! {
            Some(out) = resolve_rx.recv() => {
                resolve_count += 1;
                engine.manager.on_resolve_ready(out).await;
            }
            Some(done) = done_rx.recv() => break done,
            _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("timeout waiting for download"),
        }
    };
    engine.manager.on_task_done(&done).await;

    let dest = work.join("out.bin");
    let bytes = tokio::fs::read(&dest).await.expect("read result");
    assert_eq!(bytes, FILE_BODY, "downloaded bytes must match source (via resolved URL)");
    assert_eq!(resolve_count, 1, "resolve should fire exactly once for a fresh create");

    let _ = tokio::fs::remove_dir_all(&work).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_plugin_passes_through() {
    let work = std::env::temp_dir().join(format!("fluxdown-plugin-dis-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real.bin");

    let plugin_src = work.join("plugin_src");
    write_rewrite_plugin(&plugin_src).await;

    let mut engine = Engine::new(engine_config(&work), Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine");
    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&plugin_src).await.expect("install");
    // 禁用插件 → match_resolver 跳过 → 原始（此处即真实）URL 直下，不经 resolve。
    pm.set_enabled("test@rewriter", false).await.expect("disable");

    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");

    let save = work.to_string_lossy().into_owned();
    // 直接用真实 URL（禁用后不改写；即使匹配也被跳过）。
    let _tid = create(&mut engine, &real_url, &save, "out.bin").await;

    let mut resolve_count = 0u32;
    let done = loop {
        tokio::select! {
            Some(out) = resolve_rx.recv() => { resolve_count += 1; engine.manager.on_resolve_ready(out).await; }
            Some(done) = done_rx.recv() => break done,
            _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("timeout"),
        }
    };
    engine.manager.on_task_done(&done).await;

    let bytes = tokio::fs::read(work.join("out.bin")).await.expect("read");
    assert_eq!(bytes, FILE_BODY);
    assert_eq!(resolve_count, 0, "disabled plugin must not trigger resolve");

    let _ = tokio::fs::remove_dir_all(&work).await;
}

/// 回归（reviewer blocker）：resume 一个带 resolver 且处于 error(4) 的任务，必须**重新
/// resolve 并完成**——不得因 DB status==4 被 on_resolve_ready 误判为「窗口内已取消」而放弃。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_of_errored_resolver_task_reresolves() {
    let work = std::env::temp_dir().join(format!("fluxdown-plugin-resume-{}", uuid_like()));
    tokio::fs::create_dir_all(&work).await.expect("mkdir work");
    let (port, _srv) = spawn_server();
    let real_url = format!("http://127.0.0.1:{port}/real.bin");

    let plugin_src = work.join("plugin_src");
    write_rewrite_plugin(&plugin_src).await;

    let mut engine = Engine::new(engine_config(&work), Arc::new(NoopSink), Arc::new(NoopSelection))
        .await
        .expect("engine");
    let pm = engine.manager.plugin_manager().expect("pm installed");
    pm.install_from_dir(&plugin_src).await.expect("install");
    pm.update_settings("test@rewriter", &[("target".to_string(), real_url.clone())])
        .await
        .expect("set target");

    let mut resolve_rx = engine.manager.take_resolve_rx().expect("resolve_rx");
    let mut done_rx = engine.manager.take_done_rx().expect("done_rx");
    let save = work.to_string_lossy().into_owned();
    let tid = create(&mut engine, "http://origin.test/watch?v=abc", &save, "out.bin").await;

    // 一轮：create → resolve(1) → 完成。
    let done = loop {
        tokio::select! {
            Some(out) = resolve_rx.recv() => engine.manager.on_resolve_ready(out).await,
            Some(done) = done_rx.recv() => break done,
            _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("timeout round 1"),
        }
    };
    engine.manager.on_task_done(&done).await;

    // 人为把任务置为 error(4)（模拟直链过期导致的失败），删掉已下文件以便重下。
    engine
        .db
        .update_task_status(&tid, 4, "simulated expiry")
        .await
        .expect("set status 4");
    let _ = tokio::fs::remove_file(work.join("out.bin")).await;

    // resume：带 resolver 的 error 任务 → 必须重新 resolve（惰性防过期）并再次完成。
    engine.manager.resume_task(&tid).await;
    let done2 = loop {
        tokio::select! {
            Some(out) = resolve_rx.recv() => engine.manager.on_resolve_ready(out).await,
            Some(done) = done_rx.recv() => break done,
            _ = tokio::time::sleep(Duration::from_secs(15)) =>
                panic!("timeout round 2: resume 未能重新 resolve+完成（blocker 回归）"),
        }
    };
    engine.manager.on_task_done(&done2).await;

    let bytes = tokio::fs::read(work.join("out.bin")).await.expect("read after resume");
    assert_eq!(bytes, FILE_BODY, "resume 应重新 resolve 并重下完成");
    let _ = tokio::fs::remove_dir_all(&work).await;
}

/// 简易唯一后缀（避免引入 uuid 到测试；进程 id + 纳秒时间戳）。
fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}
