//! 全局文件日志 — 与 Dart 端 LogService 写入同一目录/文件，按日期分文件。
//!
//! - 日志目录：Linux `~/.local/share/fluxdown/logs/`，Windows exe 同级 `logs/`
//! - 文件名：`fluxdown_YYYY-MM-DD.log`（与 Dart 端完全一致）
//! - 两端都以 append 模式写入，POSIX `O_APPEND` 保证单次 write 原子性
//! - 启动时自动清理 7 天前的日志文件
//!
//! ## 用法
//! ```ignore
//! // 初始化（Rust runtime 启动时调用一次）
//! crate::logger::init();
//!
//! // 普通日志
//! log_info!("[module] some message: {}", value);
//!
//! // 错误日志（立即刷盘）
//! log_error!("[module] failed: {}", err);
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use chrono::Local;

static LOGGER: OnceLock<AppLogger> = OnceLock::new();

/// 日志保留天数
const LOG_RETENTION_DAYS: u64 = 7;

struct LogState {
    date_tag: String,
    file: Option<File>,
}

struct AppLogger {
    log_dir: PathBuf,
    state: Mutex<LogState>,
}

impl AppLogger {
    fn new(log_dir: PathBuf) -> Self {
        fs::create_dir_all(&log_dir).ok();
        Self {
            log_dir,
            state: Mutex::new(LogState {
                date_tag: String::new(),
                file: None,
            }),
        }
    }

    // ── 内部写入 ──

    /// 写入一行日志，自动按日期切换文件。`flush` 为 true 时立即刷盘。
    fn write_impl(&self, message: &str, flush: bool) {
        let now = Local::now();
        let date_tag = now.format("%Y-%m-%d").to_string();
        let ts = now.format("%H:%M:%S%.3f").to_string();
        let line = format!("{ts} {message}\n");

        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };

        self.ensure_file(&mut state, &date_tag);

        if let Some(ref mut f) = state.file {
            let _ = f.write_all(line.as_bytes());
            if flush {
                let _ = f.flush();
            }
        }
    }

    /// 确保日志文件已打开且日期匹配，否则切换到新文件。
    fn ensure_file(&self, state: &mut LogState, date_tag: &str) {
        if state.date_tag == date_tag && state.file.is_some() {
            return;
        }
        // 关闭旧文件（如有）
        if let Some(ref mut old) = state.file {
            let _ = old.flush();
        }
        state.file = None;

        let path = self.log_dir.join(format!("fluxdown_{date_tag}.log"));
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                state.date_tag = date_tag.to_string();
                state.file = Some(f);
            }
            Err(_) => {}
        }
    }

    /// 写入启动 header
    fn write_session_header(&self) {
        let now = Local::now();
        let header = format!(
            "\n====== Rust runtime log session started at {} ======\n  pid: {}\n  exe: {}\n\n",
            now.format("%Y-%m-%d %H:%M:%S"),
            std::process::id(),
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
        );

        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };

        let date_tag = now.format("%Y-%m-%d").to_string();
        self.ensure_file(&mut state, &date_tag);

        if let Some(ref mut f) = state.file {
            let _ = f.write_all(header.as_bytes());
            let _ = f.flush();
        }
    }

    /// 清理超过 `max_days` 天的 `fluxdown_*.log` 文件
    fn cleanup_old_logs(&self, max_days: u64) {
        let cutoff = SystemTime::now() - Duration::from_secs(max_days * 86400);
        let entries = match fs::read_dir(&self.log_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("fluxdown_") || !name.ends_with(".log") {
                continue;
            }
            if let Ok(meta) = fs::metadata(&path) {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

// ══════════════════════════════════════════════════
//  公开 API
// ══════════════════════════════════════════════════

/// 初始化全局日志。应在 Rust runtime 启动时调用一次。
///
/// 自动清理 7 天前的日志文件，并写入 session header。
pub fn init() {
    let log_dir = resolve_log_dir();
    let logger = AppLogger::new(log_dir);
    logger.cleanup_old_logs(LOG_RETENTION_DAYS);
    if LOGGER.set(logger).is_ok() {
        if let Some(l) = LOGGER.get() {
            l.write_session_header();
        }
    }
}

/// 写入一条日志（缓冲写入，由 OS 按需刷盘）。
#[inline]
pub fn write(message: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.write_impl(message, false);
    }
    #[cfg(debug_assertions)]
    eprintln!("{message}");
}

/// 写入一条错误日志（立即刷盘，确保崩溃前不丢失）。
#[inline]
#[allow(dead_code)]
pub fn write_error(message: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.write_impl(message, true);
    }
    #[cfg(debug_assertions)]
    eprintln!("{message}");
}

// ══════════════════════════════════════════════════
//  路径解析 — 与 Dart LogService._resolveLogDir() 一致
// ══════════════════════════════════════════════════

fn resolve_log_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let xdg = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.local/share")
        });
        PathBuf::from(format!("{xdg}/fluxdown/logs"))
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(format!("{home}/Library/Application Support/fluxdown/logs"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let exe = std::env::current_exe().unwrap_or_default();
        exe.parent()
            .unwrap_or(std::path::Path::new("."))
            .join("logs")
    }
}

// ══════════════════════════════════════════════════
//  宏 — 直接替换 rinf::debug_print!
// ══════════════════════════════════════════════════

/// 记录普通日志，格式同 `format!()`。
///
/// ```ignore
/// log_info!("[actor] task created: id={}", id);
/// ```
macro_rules! log_info {
    ($($arg:tt)*) => {
        crate::logger::write(&format!($($arg)*))
    };
}

/// 记录错误日志（立即刷盘），格式同 `format!()`。
///
/// ```ignore
/// log_error!("[actor] database open failed: {}", e);
/// ```
#[allow(unused_macros)]
macro_rules! log_error {
    ($($arg:tt)*) => {
        crate::logger::write_error(&format!($($arg)*))
    };
}

#[allow(unused_imports)]
pub(crate) use log_error;
pub(crate) use log_info;
