use std::path::PathBuf;

use rinf::{DartSignal, RustSignal};
use tokio::sync::mpsc;

use crate::bt_downloader::{self, BtConfig};
use crate::db::Db;
use crate::download_manager::{self, DownloadManager, TaskDone};
use crate::native_messaging::{self};
use crate::signals::{
    BatchCreateTask, CheckForUpdate, ConfigEntry, ConfigLoaded, ConfirmExternalDownload,
    ControlTask, CreateTask, DownloadUpdate, ExternalDownloadRequest, InstallUpdate,
    RequestAllTasks, RequestConfig, SaveConfig, UpdateCheckResult,
};
use crate::updater;

/// Compute default save directory (platform-dependent).
fn default_save_dir() -> String {
    if cfg!(target_os = "windows")
        && let Some(profile) = std::env::var_os("USERPROFILE")
    {
        let mut p = PathBuf::from(profile);
        p.push("Downloads");
        return p.to_string_lossy().into_owned();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push("Downloads");
        return p.to_string_lossy().into_owned();
    }
    ".".to_string()
}

/// Read initial config values from DB to pass to DownloadManager.
async fn load_initial_config(db: &Db) -> (usize, u64, String, BtConfig) {
    let config = db.get_all_config().await.unwrap_or_default();

    let max_concurrent = config
        .get("max_concurrent_tasks")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);

    let speed_limit_bytes = config
        .get("speed_limit_bytes")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let save_dir = config
        .get("default_save_dir")
        .cloned()
        .unwrap_or_else(default_save_dir);

    let bt_config = BtConfig {
        enable_dht: config
            .get("bt_enable_dht")
            .map(|v| v == "true")
            .unwrap_or(true),
        enable_upnp: config
            .get("bt_enable_upnp")
            .map(|v| v == "true")
            .unwrap_or(true),
        port_start: config
            .get("bt_port_start")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6881),
        port_end: config
            .get("bt_port_end")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(6891),
        custom_trackers: config
            .get("bt_custom_trackers")
            .cloned()
            .unwrap_or_default(),
    };

    (max_concurrent, speed_limit_bytes, save_dir, bt_config)
}

pub async fn run(db_dir: PathBuf) {
    let db = match Db::open(&db_dir) {
        Ok(db) => db,
        Err(e) => {
            rinf::debug_print!("Failed to open database: {}", e);
            return;
        }
    };

    // Initialize default config values in DB (no-op if already set)
    if let Err(e) = db.init_default_config(&default_save_dir()).await {
        rinf::debug_print!("Failed to init default config: {}", e);
    }

    // Load persisted config to initialize the manager with correct limits.
    let (max_concurrent, speed_limit_bps, save_dir, mut bt_config) = load_initial_config(&db).await;

    // Populate default tracker list on first launch (when DB value is empty).
    if bt_config.custom_trackers.trim().is_empty() {
        let defaults = bt_downloader::default_tracker_list();
        if let Err(e) = db.set_config("bt_custom_trackers", &defaults).await {
            rinf::debug_print!("[actor] failed to save default trackers: {}", e);
        }
        bt_config.custom_trackers = defaults;
    }
    rinf::debug_print!(
        "[actor] init config: max_concurrent={}, speed_limit_bps={}, save_dir={}, bt_config={:?}",
        max_concurrent,
        speed_limit_bps,
        save_dir,
        bt_config,
    );

    let mut manager = match DownloadManager::new(db.clone(), max_concurrent, speed_limit_bps, save_dir, bt_config) {
        Ok(m) => m,
        Err(e) => {
            rinf::debug_print!("Failed to create download manager: {}", e);
            return;
        }
    };

    if let Some(rx) = manager.take_progress_rx() {
        tokio::spawn(download_manager::progress_reporter(rx, db.clone()));
    }

    // Channel for spawned tasks to notify completion (for active_tokens cleanup)
    let mut done_rx: mpsc::Receiver<TaskDone> = match manager.take_done_rx() {
        Some(rx) => rx,
        None => {
            // Should never happen — take_done_rx returns Some on first call
            let (_tx, rx) = mpsc::channel(1);
            rx
        }
    };

    let create_recv = CreateTask::get_dart_signal_receiver();
    let batch_create_recv = BatchCreateTask::get_dart_signal_receiver();
    let control_recv = ControlTask::get_dart_signal_receiver();
    let all_recv = RequestAllTasks::get_dart_signal_receiver();
    let config_save_recv = SaveConfig::get_dart_signal_receiver();
    let config_req_recv = RequestConfig::get_dart_signal_receiver();
    let confirm_ext_recv = ConfirmExternalDownload::get_dart_signal_receiver();
    let check_update_recv = CheckForUpdate::get_dart_signal_receiver();
    let download_update_recv = DownloadUpdate::get_dart_signal_receiver();
    let install_update_recv = InstallUpdate::get_dart_signal_receiver();

    // Spawn the Native Messaging listener (reads from stdin in a blocking thread).
    // When the browser extension sends a download request, it arrives on this channel.
    let mut native_msg_rx = native_messaging::spawn_native_messaging_listener();

    loop {
        tokio::select! {
            Some(signal) = create_recv.recv() => {
                let msg = signal.message;
                manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies)
                    .await;
            }
            Some(signal) = batch_create_recv.recv() => {
                let msg = signal.message;
                rinf::debug_print!(
                    "[actor] batch create: {} URLs, save_dir={}, segments={}",
                    msg.urls.len(), msg.save_dir, msg.segments,
                );
                for url in msg.urls {
                    manager
                        .create_task(url, msg.save_dir.clone(), String::new(), msg.segments, String::new())
                        .await;
                }
            }
            Some(signal) = control_recv.recv() => {
                let msg = signal.message;
                match msg.action {
                    0 => manager.pause_task(&msg.task_id).await,
                    1 => manager.resume_task(&msg.task_id).await,
                    2 => manager.cancel_task(&msg.task_id).await,
                    3 => manager.delete_task(&msg.task_id, true).await,   // delete record + files
                    4 => manager.delete_task(&msg.task_id, false).await,  // delete record only
                    _ => {}
                }
            }
            Some(_) = all_recv.recv() => {
                manager.load_and_send_all_tasks().await;
            }
            Some(signal) = config_save_recv.recv() => {
                let msg = signal.message;
                // Persist to DB first.
                if let Err(e) = db.set_config(&msg.key, &msg.value).await {
                    rinf::debug_print!("Failed to save config: {}", e);
                }
                // Notify DownloadManager for runtime-effective settings.
                match msg.key.as_str() {
                    "max_concurrent_tasks" => {
                        if let Ok(v) = msg.value.parse::<usize>() {
                            rinf::debug_print!("[actor] updating max_concurrent to {}", v);
                            manager.set_max_concurrent(v).await;
                        }
                    }
                    "speed_limit_bytes" => {
                        if let Ok(v) = msg.value.parse::<u64>() {
                            rinf::debug_print!("[actor] updating speed_limit to {} B/s", v);
                            manager.set_speed_limit(v);
                        }
                    }
                    "default_save_dir" => {
                        rinf::debug_print!("[actor] updating default_save_dir to {}", msg.value);
                        manager.set_default_save_dir(msg.value);
                    }
                    // BT config keys — update in-memory BtConfig and invalidate
                    // the current session so the next BT download picks up changes.
                    "bt_enable_dht" | "bt_enable_upnp" | "bt_port_start"
                    | "bt_port_end" | "bt_custom_trackers" => {
                        rinf::debug_print!("[actor] BT config changed: {}={}", msg.key, msg.value);
                        // Reload the full BT config from DB to stay consistent.
                        let all_cfg = db.get_all_config().await.unwrap_or_default();
                        let new_bt = BtConfig {
                            enable_dht: all_cfg
                                .get("bt_enable_dht")
                                .map(|v| v == "true")
                                .unwrap_or(true),
                            enable_upnp: all_cfg
                                .get("bt_enable_upnp")
                                .map(|v| v == "true")
                                .unwrap_or(true),
                            port_start: all_cfg
                                .get("bt_port_start")
                                .and_then(|v| v.parse::<u16>().ok())
                                .unwrap_or(6881),
                            port_end: all_cfg
                                .get("bt_port_end")
                                .and_then(|v| v.parse::<u16>().ok())
                                .unwrap_or(6891),
                            custom_trackers: all_cfg
                                .get("bt_custom_trackers")
                                .cloned()
                                .unwrap_or_default(),
                        };
                        manager.set_bt_config(new_bt);
                        // Invalidate (destroy) the current BT session so it is
                        // re-created with the new config on next BT download.
                        manager.invalidate_bt_session().await;
                    }
                    _ => {} // other config keys — no runtime action needed
                }
            }
            Some(_) = config_req_recv.recv() => {
                match db.get_all_config().await {
                    Ok(map) => {
                        let entries: Vec<ConfigEntry> = map
                            .into_iter()
                            .map(|(key, value)| ConfigEntry { key, value })
                            .collect();
                        ConfigLoaded { entries }.send_signal_to_dart();
                    }
                    Err(e) => {
                        rinf::debug_print!("Failed to load config: {}", e);
                    }
                }
            }
            // --- Native Messaging: browser extension download requests ---
            Some(req) = native_msg_rx.recv() => {
                rinf::debug_print!(
                    "[actor] external download request from browser: url={}, cookies_len={}",
                    req.url,
                    req.cookies.len()
                );
                // Forward to Dart UI so it can pop the quick-download dialog.
                ExternalDownloadRequest {
                    url: req.url,
                    filename: req.filename,
                    referrer: req.referrer,
                    file_size: req.file_size.map(|s| s as i64).unwrap_or(0),
                    mime_type: req.mime_type.unwrap_or_default(),
                    cookies: req.cookies,
                }
                .send_signal_to_dart();
            }
            // --- Dart confirmed an external download request ---
            Some(signal) = confirm_ext_recv.recv() => {
                let msg = signal.message;
                rinf::debug_print!(
                    "[actor] user confirmed external download: url={}, cookies_len={}",
                    msg.url,
                    msg.cookies.len()
                );
                manager
                    .create_task(msg.url, msg.save_dir, msg.file_name, msg.segments, msg.cookies)
                    .await;
            }
            Some(done) = done_rx.recv() => {
                manager.on_task_done(&done.task_id, done.generation).await;
            }
            // --- Auto-update signals ---
            Some(signal) = check_update_recv.recv() => {
                let version = signal.message.current_version;
                tokio::spawn(async move {
                    let result = std::panic::AssertUnwindSafe(
                        updater::check(&version)
                    );
                    if futures_util::FutureExt::catch_unwind(result).await.is_err() {
                        rinf::debug_print!("[updater] check panicked for version={}", version);
                        UpdateCheckResult {
                            has_update: false,
                            latest_version: String::new(),
                            current_version: version,
                            download_url: String::new(),
                            file_size: 0,
                            published_at: String::new(),
                            error_message: "internal error (panic)".to_string(),
                        }.send_signal_to_dart();
                    }
                });
            }
            Some(signal) = download_update_recv.recv() => {
                let url = signal.message.url;
                let version = signal.message.version;
                tokio::spawn(async move {
                    updater::download(&url, &version).await;
                });
            }
            Some(signal) = install_update_recv.recv() => {
                let path = signal.message.installer_path;
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = updater::install(&path) {
                        rinf::debug_print!("[updater] install error: {}", e);
                    }
                });
            }
        }
    }
}
