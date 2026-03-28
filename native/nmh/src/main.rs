//! FluxDown Native Messaging Host (NMH) relay binary.
//!
//! Chrome/Edge/Firefox launches this process when the browser extension calls
//! `chrome.runtime.connectNative("com.fluxdown.nmh")`.
//!
//! Communication flow:
//!   Browser extension <-(stdin/stdout, 4-byte LE length + JSON)-> this process
//!   this process <-(Named Pipe, 4-byte LE length + JSON)-> FluxDown App
//!
//! Design:
//!   - Synchronous, single-threaded, no async runtime.
//!   - Pipe connection is lazy: established on first message, reconnected on error.
//!   - When the FluxDown App is not running, NMH automatically launches it and
//!     polls for the Named Pipe with increasing intervals (100→200→400ms…).
//!   - "ping" messages only check connectivity — they never launch the App.
//!   - Diagnostic log is written to `%TEMP%/fluxdown_nmh.log`.
//!   - Message size limit: 1 MB (Chrome NMH hard limit).

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Maximum message size: 1 MB (Chrome NMH limit).
const MAX_MESSAGE_SIZE: u32 = 1024 * 1024;

/// IPC path for communicating with the FluxDown desktop app.
/// Windows uses a Named Pipe; Linux/macOS uses a Unix Domain Socket.
#[cfg(windows)]
const PIPE_NAME: &str = r"\\.\pipe\fluxdown";

/// FluxDown App executable name (Windows only).
#[cfg(windows)]
const APP_EXE_NAME: &str = "flux_down.exe";

/// Maximum time (ms) to wait for the App to start and create its pipe.
const APP_LAUNCH_TIMEOUT_MS: u64 = 10_000;

/// Initial polling interval after launching the App (ms).
/// Doubles after each attempt: 100 → 200 → 400 → 800 → …
const PIPE_POLL_INITIAL_MS: u64 = 100;

/// Minimum cooldown (ms) between two App launch attempts.
/// Prevents crash-loops if the App crashes on start.
const APP_LAUNCH_COOLDOWN_MS: u64 = 15_000;

/// Incoming message from the browser extension.
#[derive(Debug, Deserialize)]
struct IncomingMessage {
    #[serde(default)]
    action: String,
    #[serde(default)]
    msg_id: u64,
}

/// Response sent back to the browser extension.
#[derive(Debug, Serialize)]
struct ErrorResponse {
    success: bool,
    message: String,
    msg_id: u64,
}

// ---------------------------------------------------------------------------
// Diagnostic logging (writes to %TEMP%/fluxdown_nmh.log)
// ---------------------------------------------------------------------------

/// Resolve the NMH log file path.
fn log_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("TEMP")
            .or_else(|_| std::env::var("TMP"))
            .ok()
            .map(|tmp| Path::new(&tmp).join("fluxdown_nmh.log"))
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, Chrome launches NMH via launchd which may not set $HOME.
        // Use home_dir() which falls back to getpwuid when $HOME is absent.
        if let Some(home) = home_dir() {
            let dir = home
                .join("Library")
                .join("Application Support")
                .join("fluxdown");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("fluxdown_nmh.log"));
        }
        Some(Path::new("/tmp").join("fluxdown_nmh.log"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        // Prefer XDG_RUNTIME_DIR (user-private); fall back to /tmp.
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            Some(Path::new(&dir).join("fluxdown_nmh.log"))
        } else {
            Some(Path::new("/tmp").join("fluxdown_nmh.log"))
        }
    }
}

/// Append a timestamped line to the NMH log file.
/// Failures are silently ignored — logging must never break the relay.
fn log(msg: &str) {
    let Some(path) = log_path() else {
        return;
    };
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };

    // Truncate to 256 KB to prevent unbounded growth.
    if let Ok(meta) = f.metadata()
        && meta.len() > 256 * 1024
    {
        let _ = f.set_len(0);
    }

    let now = chrono_free_timestamp();
    let _ = writeln!(f, "[{now}] {msg}");
}

/// Simple timestamp without pulling in chrono — "YYYY-MM-DD HH:MM:SS".
fn chrono_free_timestamp() -> String {
    // Use std::time for elapsed since NMH start; not wall-clock but cheap.
    // For wall-clock we'd need `chrono` or Win32 GetLocalTime. Keep it simple.
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    // UTC is fine for diagnostics; avoids timezone complexity.
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

// ---------------------------------------------------------------------------
// Home directory resolution (macOS: getpwuid fallback for launchd env)
// ---------------------------------------------------------------------------

/// Returns the current user's home directory.
///
/// On macOS, Chrome/Firefox launch the NMH process via launchd, which may
/// strip $HOME from the environment. Fall back to the passwd database via
/// getpwuid(getuid()) to get the correct home directory in all cases.
#[cfg(target_os = "macos")]
fn home_dir() -> Option<PathBuf> {
    // Fast path: $HOME is set (terminal / direct invocation).
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }

    // Slow path: query the passwd database. Safe to call from any thread
    // because we use the re-entrant getpwuid_r variant.
    use std::ffi::CStr;
    let uid = unsafe { libc::getuid() };
    let buf_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buf_size = if buf_size > 0 {
        buf_size as usize
    } else {
        1024
    };
    let mut buf = vec![0i8; buf_size];
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result: *mut libc::passwd = std::ptr::null_mut();

    let ret = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buf.as_mut_ptr(),
            buf_size,
            &mut result,
        )
    };

    if ret == 0 && !result.is_null() {
        let pwd = unsafe { pwd.assume_init() };
        if !pwd.pw_dir.is_null() {
            let cstr = unsafe { CStr::from_ptr(pwd.pw_dir) };
            if let Ok(s) = cstr.to_str() {
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// stdin/stdout helpers (4-byte LE length-prefixed JSON, per NMH protocol)
// ---------------------------------------------------------------------------

/// Read one NMH message from stdin.
/// Returns `None` on EOF (extension disconnected).
fn read_stdin_message() -> Option<Vec<u8>> {
    let stdin = io::stdin();
    let mut handle = stdin.lock();

    let mut len_buf = [0u8; 4];
    if handle.read_exact(&mut len_buf).is_err() {
        return None;
    }
    let len = u32::from_le_bytes(len_buf);
    if len == 0 || len > MAX_MESSAGE_SIZE {
        return None;
    }

    let mut buf = vec![0u8; len as usize];
    if handle.read_exact(&mut buf).is_err() {
        return None;
    }
    Some(buf)
}

/// Write one NMH message to stdout.
fn write_stdout_message(data: &[u8]) {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let len = data.len() as u32;
    let _ = handle.write_all(&len.to_le_bytes());
    let _ = handle.write_all(data);
    let _ = handle.flush();
}

// ---------------------------------------------------------------------------
// Named Pipe helpers (Windows)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod pipe {
    use std::fs::OpenOptions;
    use std::io::{self, Read, Write};

    pub struct PipeHandle {
        file: std::fs::File,
    }

    impl PipeHandle {
        pub fn connect(pipe_name: &str) -> Option<Self> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe_name)
                .ok()?;
            Some(PipeHandle { file })
        }

        pub fn write_message(&mut self, data: &[u8]) -> io::Result<()> {
            let len = data.len() as u32;
            self.file.write_all(&len.to_le_bytes())?;
            self.file.write_all(data)?;
            // NOTE: flush() is intentionally omitted.
            // On Windows Named Pipes, File::flush() calls FlushFileBuffers(), which
            // BLOCKS until the remote end reads all data. If the Tokio async server
            // hasn't scheduled its read yet, this deadlocks for ~17 seconds until
            // Windows aborts the I/O. Named pipe writes go to the kernel buffer
            // immediately — no explicit flush is needed.
            Ok(())
        }

        pub fn read_message(&mut self) -> io::Result<Vec<u8>> {
            let mut len_buf = [0u8; 4];
            self.file.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf);
            if len > super::MAX_MESSAGE_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "message too large",
                ));
            }
            let mut buf = vec![0u8; len as usize];
            self.file.read_exact(&mut buf)?;
            Ok(buf)
        }
    }
}

// Non-Windows: connect to FluxDown via Unix Domain Socket.
#[cfg(not(windows))]
mod pipe {
    use std::io::{self, Read, Write};
    use std::os::unix::net::UnixStream;

    /// Resolve the Unix socket path that the FluxDown app is listening on.
    /// Must match the path used in native/hub/src/native_messaging.rs.
    fn socket_path() -> std::path::PathBuf {
        #[cfg(target_os = "macos")]
        {
            // macOS: ~/Library/Application Support/fluxdown/fluxdown.sock
            // Must match native/hub/src/native_messaging.rs socket_path().
            // Use home_dir() (getpwuid fallback) instead of $HOME directly,
            // because Chrome/Firefox launch NMH via launchd which strips $HOME.
            if let Some(home) = super::home_dir() {
                let dir = home
                    .join("Library")
                    .join("Application Support")
                    .join("fluxdown");
                let _ = std::fs::create_dir_all(&dir);
                return dir.join("fluxdown.sock");
            }
        }
        // Linux (and any other Unix-like fallback)
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            std::path::Path::new(&dir).join("fluxdown.sock")
        } else {
            std::path::Path::new("/tmp").join("fluxdown.sock")
        }
    }

    pub struct PipeHandle {
        stream: UnixStream,
    }

    impl PipeHandle {
        /// Connect to the FluxDown Unix socket. Returns None if the app is not running.
        pub fn connect(_ignored: &str) -> Option<Self> {
            let path = socket_path();
            let stream = UnixStream::connect(&path).ok()?;
            Some(PipeHandle { stream })
        }

        pub fn write_message(&mut self, data: &[u8]) -> io::Result<()> {
            let len = data.len() as u32;
            self.stream.write_all(&len.to_le_bytes())?;
            self.stream.write_all(data)?;
            self.stream.flush()?;
            Ok(())
        }

        pub fn read_message(&mut self) -> io::Result<Vec<u8>> {
            let mut len_buf = [0u8; 4];
            self.stream.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf);
            if len > super::MAX_MESSAGE_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "message too large",
                ));
            }
            let mut buf = vec![0u8; len as usize];
            self.stream.read_exact(&mut buf)?;
            Ok(buf)
        }
    }
}

// ---------------------------------------------------------------------------
// App auto-launch
// ---------------------------------------------------------------------------

/// Find the FluxDown App executable.
///
/// Search order:
/// 1. Same directory as NMH exe (production + CMake-embedded dev builds)
/// 2. Flutter build output (development fallback)
#[cfg(windows)]
fn find_app_exe() -> Option<PathBuf> {
    // 1. Same directory as NMH exe
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(APP_EXE_NAME);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. Flutter build output (development fallback)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent());

    if let Some(ws) = workspace_root {
        for arch in &["x64", "arm64"] {
            for profile in &["Debug", "Release", "Profile"] {
                let candidate = ws
                    .join("build")
                    .join("windows")
                    .join(arch)
                    .join("runner")
                    .join(profile)
                    .join(APP_EXE_NAME);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn find_app_exe() -> Option<PathBuf> {
    // macOS app exe name depends on PRODUCT_NAME in Xcode / AppInfo.xcconfig.
    // flutter run / flutter build macos uses the display name ("FluxDown"),
    // while production archives may differ. Search both variants.
    const APP_EXE_CANDIDATES: &[&str] = &["FluxDown", "flux_down"];
    const APP_BUNDLE_CANDIDATES: &[&str] = &["FluxDown.app", "flux_down.app"];

    // 1. Same directory as NMH binary (inside .app bundle: Contents/MacOS/)
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for name in APP_EXE_CANDIDATES {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 2. Flutter build output (development — flutter build macos / flutter run)
    //    build/macos/Build/Products/{Debug,Release,Profile}/<AppName>.app/Contents/MacOS/<AppName>
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent());

    if let Some(ws) = workspace_root {
        for profile_cap in &["Debug", "Release", "Profile"] {
            let products = ws
                .join("build")
                .join("macos")
                .join("Build")
                .join("Products")
                .join(profile_cap);
            for bundle in APP_BUNDLE_CANDIDATES {
                for exe_name in APP_EXE_CANDIDATES {
                    let candidate = products
                        .join(bundle)
                        .join("Contents")
                        .join("MacOS")
                        .join(exe_name);
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    None
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn find_app_exe() -> Option<PathBuf> {
    // 1. Same directory as NMH binary (production deployment)
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("flux_down");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. Flutter build output (development — flutter run / flutter build linux)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = Path::new(manifest_dir).parent().and_then(|p| p.parent());

    if let Some(ws) = workspace_root {
        for profile in &["debug", "release", "profile"] {
            let candidate = ws
                .join("build")
                .join("linux")
                .join("x64")
                .join(profile)
                .join("bundle")
                .join("flux_down");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Launch the FluxDown App as a detached process.
#[cfg(windows)]
fn launch_app(app_exe: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

    std::process::Command::new(app_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .is_ok()
}

#[cfg(not(windows))]
fn launch_app(app_exe: &Path) -> bool {
    std::process::Command::new(app_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Returns the IPC address string for `pipe::PipeHandle::connect()`.
/// On Windows this is the Named Pipe path; on non-Windows the argument is
/// ignored and the Unix socket path is resolved inside the `pipe` module.
fn ipc_address() -> &'static str {
    #[cfg(windows)]
    {
        PIPE_NAME
    }
    #[cfg(not(windows))]
    {
        // Unix socket path is computed from $XDG_RUNTIME_DIR inside pipe::PipeHandle::connect.
        ""
    }
}

/// Try to connect to the IPC endpoint. If unavailable, launch the App
/// (subject to cooldown) and poll with exponential back-off until the
/// endpoint appears or the timeout is reached.
fn connect_with_auto_launch(last_launch: &mut Option<Instant>) -> Option<pipe::PipeHandle> {
    let addr = ipc_address();

    // Fast path: App is already running.
    if let Some(p) = pipe::PipeHandle::connect(addr) {
        log("ipc connected (fast path)");
        return Some(p);
    }

    // Cooldown: don't re-launch too quickly (prevents crash-loop).
    if let Some(prev) = last_launch
        && prev.elapsed().as_millis() < APP_LAUNCH_COOLDOWN_MS as u128
    {
        log("launch skipped: cooldown active");
        return None;
    }

    // Find and launch the App.
    let app_exe = match find_app_exe() {
        Some(p) => p,
        None => {
            log("App exe not found");
            return None;
        }
    };

    log(&format!("launching App: {}", app_exe.display()));
    if !launch_app(&app_exe) {
        log("App launch failed (spawn error)");
        return None;
    }
    *last_launch = Some(Instant::now());

    // Poll with exponential back-off: 100 → 200 → 400 → 800 → 1000(cap) → …
    let deadline = Instant::now() + std::time::Duration::from_millis(APP_LAUNCH_TIMEOUT_MS);
    let mut interval = PIPE_POLL_INITIAL_MS;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(interval));

        if let Some(p) = pipe::PipeHandle::connect(addr) {
            let elapsed = last_launch.map_or(0, |t| t.elapsed().as_millis() as u64);
            log(&format!("ipc connected after {}ms", elapsed));
            return Some(p);
        }

        if Instant::now() >= deadline {
            break;
        }

        // Cap interval at 1000ms to keep responsiveness.
        interval = (interval * 2).min(1000);
    }

    log("ipc connect timed out after launch");
    None
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn main() {
    log("NMH started");

    let mut pipe: Option<pipe::PipeHandle> = None;
    let mut last_launch: Option<Instant> = None;

    while let Some(raw) = read_stdin_message() {
        let parsed = serde_json::from_slice::<IncomingMessage>(&raw);
        let msg_id = parsed.as_ref().map_or(0, |m| m.msg_id);
        let is_ping = parsed.as_ref().is_ok_and(|m| m.action == "ping");

        // Ensure IPC connection.
        // "ping" only does a direct connect (no App launch for status checks).
        if pipe.is_none() {
            pipe = if is_ping {
                pipe::PipeHandle::connect(ipc_address())
            } else {
                connect_with_auto_launch(&mut last_launch)
            };
        }

        let Some(ref mut p) = pipe else {
            let resp = ErrorResponse {
                success: false,
                message: "app_not_running".to_string(),
                msg_id,
            };
            if let Ok(json) = serde_json::to_vec(&resp) {
                write_stdout_message(&json);
            }
            continue;
        };

        // Forward message to App via Named Pipe.
        if let Err(e) = p.write_message(&raw) {
            log(&format!("pipe write failed ({}), dropping connection", e));
            pipe = None;
            let resp = ErrorResponse {
                success: false,
                message: "app_not_running".to_string(),
                msg_id,
            };
            if let Ok(json) = serde_json::to_vec(&resp) {
                write_stdout_message(&json);
            }
            continue;
        }

        // Read response from App.
        match p.read_message() {
            Ok(response_data) => {
                write_stdout_message(&response_data);
            }
            Err(e) => {
                log(&format!("pipe read failed ({}), dropping connection", e));
                pipe = None;
                let resp = ErrorResponse {
                    success: false,
                    message: "app_not_running".to_string(),
                    msg_id,
                };
                if let Ok(json) = serde_json::to_vec(&resp) {
                    write_stdout_message(&json);
                }
            }
        }
    }

    log("NMH exiting (stdin closed)");
}
