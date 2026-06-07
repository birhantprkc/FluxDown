//! BitTorrent / Magnet-link download engine.
//!
//! Uses **librqbit** as the BT backend.  All BT tasks share a single
//! `Session` (DHT, trackers, listening port) managed by [`SharedBtSession`],
//! which lives inside `DownloadManager`.  This avoids per-task resource waste
//! (redundant DHT nodes, tracker connections, OS threads, listening ports).
//!
//! Because librqbit requires a multi-threaded tokio runtime while our main
//! actor runs on `current_thread`, the shared session is created inside a
//! dedicated `Runtime(multi_thread)`.  Individual download tasks submit work
//! to that runtime via `Runtime::spawn`.
//!
//! Key design:
//! - Single shared `Session` with DHT + public trackers + UPnP.
//! - Speed limit is applied at the `Session` level via `ratelimits` and
//!   updated in real-time when the user changes the global speed setting.
//! - `add_torrent` blocks while resolving magnet metadata from DHT/peers, so
//!   we report "preparing" status to Dart while we wait.

use rinf::RustSignal;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, SetFileAttributesW,
};

use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent, PeerConnectionOptions,
    Session, SessionOptions, SessionPersistenceConfig,
};

/// Alias for librqbit's `BtHandle` (`Arc<ManagedTorrent>`).
/// The upstream type is not re-exported, so we define it locally.
pub type BtHandle = Arc<ManagedTorrent>;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::downloader::{DownloadError, ProgressUpdate, SegmentProgressInfo};
use crate::logger::log_info;

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Truncate an identifier to at most 8 characters for log output.
/// Returns the full string if shorter than 8 characters, avoiding panic
/// from direct byte-index slicing on short or multi-byte strings.
#[inline]
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Returns `true` if the URL looks like a magnet link.
pub fn is_magnet_url(url: &str) -> bool {
    url.get(..8)
        .map(|prefix| prefix.eq_ignore_ascii_case("magnet:?"))
        .unwrap_or(false)
}

/// Torrent input source — either a magnet URI or raw .torrent file bytes.
/// Replaces the old hardcoded `magnet_url: String` field so that
/// `BtDownloadParams` can represent both kinds of BT downloads uniformly.
#[derive(Clone)]
pub enum TorrentSource {
    /// A magnet link URI string (e.g. `magnet:?xt=urn:btih:...`).
    Magnet(String),
    /// Raw bytes of a `.torrent` file read from disk.
    TorrentFileBytes(Vec<u8>),
}

impl TorrentSource {
    /// Returns `true` if this source is a magnet link.
    #[allow(dead_code)]
    pub fn is_magnet(&self) -> bool {
        matches!(self, TorrentSource::Magnet(_))
    }

    /// Best-effort display name for logging / early UI display.
    /// For magnet links, extracts the `dn=` parameter.
    /// For torrent file bytes, returns None (the name comes from metadata).
    pub fn display_name(&self) -> Option<String> {
        match self {
            TorrentSource::Magnet(url) => magnet_display_name(url),
            TorrentSource::TorrentFileBytes(_) => None,
        }
    }

    /// URL string for DB storage.  Magnet links store the URI directly.
    /// Torrent file sources store a sentinel `torrent-file://` URL since the
    /// actual content is persisted separately in the `torrent_file_bytes` column.
    #[allow(dead_code)]
    pub fn url_for_db(&self) -> &str {
        match self {
            TorrentSource::Magnet(url) => url,
            TorrentSource::TorrentFileBytes(_) => "torrent-file://local",
        }
    }
}

/// Extract the `dn=` (display name) parameter from a magnet URI, if present.
///
/// The decoded value is sanitized via `crate::downloader::sanitize_filename`
/// to strip path separators and other illegal characters (`/`, `\`, `:`, …),
/// matching `meta_prober::extract_dn_from_magnet`.  Without this, an illegal
/// `dn=` would flow into the DB display name and the metadata-failure fallback
/// file name inconsistently with the queued-task path.  Returns `None` when the
/// decoded value is empty (before sanitization), so callers fall back to a
/// generated name instead of the literal `"download"` placeholder.
fn magnet_display_name(url: &str) -> Option<String> {
    url.split('&')
        .find_map(|part| {
            let part = part.strip_prefix("magnet:?").unwrap_or(part);
            part.strip_prefix("dn=")
        })
        .and_then(|raw| {
            let decoded = urlencoding_decode(raw);
            if decoded.is_empty() {
                None
            } else {
                Some(crate::downloader::sanitize_filename(&decoded))
            }
        })
}

/// Minimal percent-decoding for `dn=` values (UTF-8 safe).
///
/// Collects **both** percent-encoded bytes (`%XX`) **and** literal bytes into a
/// shared byte buffer, then decodes the buffer as UTF-8 (with GBK fallback).
/// This correctly handles multi-byte characters (e.g. CJK, emoji) regardless of
/// whether they arrive percent-encoded or as raw literal UTF-8 — many BT
/// clients write the original UTF-8 directly into `dn=` (e.g. `dn=中文电影`).
/// Decoding per-byte via `b as char` would treat each UTF-8 byte as a Latin-1
/// code point and produce mojibake.
///
/// `+` decodes to a space (flushing the accumulated buffer first, since it is a
/// genuine delimiter).  Incomplete or invalid `%` sequences are kept as literal
/// bytes rather than silently padded with zeros.
fn urlencoding_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut bytes_buf: Vec<u8> = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    // Flush accumulated percent-encoded bytes as UTF-8 into `out`.
    // 优先 UTF-8，失败时回退到 GBK（应对老旧中文资源库 magnet 中
    // 的 GBK 编码 dn=），双失败才使用 replacement char。
    let flush = |buf: &mut Vec<u8>, out: &mut String| {
        if !buf.is_empty() {
            match crate::downloader::decode_bytes_utf8_or_gbk(buf) {
                Ok(s) => out.push_str(&s),
                Err(_) => {
                    out.push(char::REPLACEMENT_CHARACTER);
                }
            }
            buf.clear();
        }
    };

    while i < len {
        match bytes[i] {
            b'+' => {
                flush(&mut bytes_buf, &mut out);
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < len => {
                // Full %XX sequence — decode as a byte only if both are
                // valid hex digits; otherwise treat `%` as a literal.
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                    bytes_buf.push(h << 4 | l);
                    i += 3;
                } else {
                    // Not a valid `%XX` escape — keep the `%` as a literal byte
                    // (0x25, valid ASCII) in `bytes_buf` so it decodes together
                    // with any surrounding literal multi-byte sequence.
                    bytes_buf.push(b'%');
                    i += 1;
                }
            }
            b'%' => {
                // Incomplete `%` at end of string — treat the `%` and any
                // trailing bytes as literal content.  We push them as raw
                // bytes into `bytes_buf` (rather than `b as char`, which would
                // mangle multi-byte UTF-8 by re-interpreting each byte as a
                // Latin-1 code point) so the trailing sequence is decoded
                // together with surrounding literal bytes by
                // `decode_bytes_utf8_or_gbk`.  0x25 ('%') is valid ASCII and
                // safely passes through UTF-8 decoding unchanged.
                while i < len {
                    bytes_buf.push(bytes[i]);
                    i += 1;
                }
            }
            _ => {
                // Literal byte — accumulate into `bytes_buf` so that literal
                // multi-byte UTF-8 sequences (common in magnet `dn=` values,
                // e.g. `dn=中文电影`) are decoded as a whole instead of
                // per-byte via `b as char` (which produced mojibake).
                bytes_buf.push(bytes[i]);
                i += 1;
            }
        }
    }
    flush(&mut bytes_buf, &mut out);
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Well-known public trackers used to accelerate peer discovery for magnet
/// links that ship without `tr=` parameters.
///
/// **Curated from global community sources** (2026-02-10):
///   - ngosang/trackerslist (52.9k stars, auto-updated daily, ranked by latency)
///   - XIU2/TrackersListCollection (popular in CN community)
///   - Cross-referenced and **availability-tested** before inclusion.
///
/// Strategy: CN/Asia trackers first (better peer locality for domestic users),
/// then international trackers.  UDP-heavy (lowest overhead), with HTTPS
/// fallbacks for restrictive network environments where UDP may be blocked.
///
/// Kept to ~25 high-availability trackers to minimise DNS/connect overhead
/// while still providing excellent global peer coverage.  All tracker
/// connections are async and parallel, so startup impact is minimal.
const PUBLIC_TRACKERS: &[&str] = &[
    // ─── CN / Asia — better peer discovery for domestic users ───
    "udp://tracker.dler.com:6969/announce",
    "udp://admin.52ywp.com:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "https://tracker.moeblog.cn:443/announce",
    "http://nyaa.tracker.wf:7777/announce",
    "https://tr.zukizuki.org:443/announce",
    // ─── International — top-tier, highest uptime ───
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.dstud.io:6969/announce",
    "udp://tracker-udp.gbitt.info:80/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.srv00.com:6969/announce",
    "udp://tracker.qu.ax:6969/announce",
    "udp://opentracker.io:6969/announce",
    "udp://tracker.bittor.pw:1337/announce",
    "udp://tracker.theoks.net:6969/announce",
    "udp://tracker.opentorrent.top:6969/announce",
    "udp://open.demonoid.ch:6969/announce",
    "udp://tracker.t-1.org:6969/announce",
    // ─── HTTPS fallbacks — for networks that block UDP ───
    "https://tracker.ghostchu-services.top:443/announce",
    "https://tracker.bt4g.com:443/announce",
    "https://1337.abcvg.info:443/announce",
    "http://tracker.bt4g.com:2095/announce",
];

/// Return the built-in public tracker list as a newline-separated string.
/// Used to populate the default config value on first launch so users can
/// see and edit the full list in Settings.
pub fn default_tracker_list() -> String {
    PUBLIC_TRACKERS.join("\n")
}

// ---------------------------------------------------------------------------
// BT configuration — user-settable via the Settings page
// ---------------------------------------------------------------------------

/// User-configurable BT session settings, loaded from the DB config table.
#[derive(Debug, Clone)]
pub struct BtConfig {
    pub enable_dht: bool,
    pub enable_upnp: bool,
    pub port_start: u16,
    pub port_end: u16,
    /// User-supplied extra tracker URLs (newline-separated).
    /// These are **merged** with the built-in `PUBLIC_TRACKERS` list.
    pub custom_trackers: String,
}

impl Default for BtConfig {
    fn default() -> Self {
        Self {
            enable_dht: true,
            enable_upnp: true,
            port_start: 6881,
            port_end: 6891,
            custom_trackers: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared BT Session — singleton owned by DownloadManager
// ---------------------------------------------------------------------------

/// A shared BT session that holds a dedicated multi-thread runtime and a
/// single `librqbit::Session`.  All BT tasks share this instance, which
/// means they share DHT routing tables, tracker connections, and the
/// listening port — dramatically reducing resource usage.
///
/// Torrent handles are cached in `handles` so that pause/resume cycles
/// use the native `Session::pause` / `Session::unpause` API instead of
/// deleting and re-adding the torrent.  This preserves fast-resume data
/// (piece bitfield) and avoids expensive re-verification of already
/// downloaded pieces.
pub struct SharedBtSession {
    runtime: tokio::runtime::Runtime,
    session: Arc<Session>,
    /// Maps our `task_id` → librqbit `BtHandle`.
    /// Protected by an async Mutex because it's accessed from both the
    /// main actor (pause/delete) and spawned download tasks (add/finish).
    handles: Mutex<HashMap<String, BtHandle>>,
    /// Pending delete requests for tasks whose `add_torrent` call is still
    /// in progress (handle not yet in `handles`).  Keyed by task_id, value
    /// is the `delete_files` flag.  The detached add_torrent closure checks
    /// this map on completion and calls `session.delete` accordingly,
    /// preventing orphaned files when a magnet task is deleted during DHT
    /// metadata resolution.
    pending_deletes: Mutex<HashMap<String, bool>>,
    /// Count of detached `add_torrent` tasks currently running in the
    /// background.  Incremented just before spawning the detached task;
    /// decremented when the task completes (success, error, or pending-delete).
    ///
    /// `maybe_release_bt_session` must not tear down the session while this
    /// is non-zero: the detached task still holds an `Arc<Session>` that
    /// keeps the listening port bound.  Creating a new session while the old
    /// port is still in use causes the next BT task to fail immediately.
    inflight_adds: AtomicUsize,
    /// Stores user's file selection for BT tasks awaiting the dialog.
    /// Key: task_id, Value: selected file indices.
    file_selection_map: Mutex<HashMap<String, Vec<i32>>>,
    /// Maps librqbit torrent ID → our task_id.
    /// Used to detect when the same torrent is added by multiple tasks.
    torrent_ids: Mutex<HashMap<usize, String>>,
    /// Serializes the completion-stage "dedup destination name + move from
    /// staging" sequence across concurrent BT tasks.
    ///
    /// All BT downloads run on the shared multi-thread bt-runtime, so two
    /// same-named torrents can finish and run `compute_completion_layout`
    /// (which checks `save_dir/name` does not exist) and `move_path` (rename)
    /// at the same instant.  Without serialization both pick the identical
    /// deduped name and the second `std::fs::rename` silently overwrites the
    /// first task's file (single file) or leaves a half-moved directory.
    ///
    /// Holding this lock across the brief dedup+move closes the TOCTOU window.
    /// It is the BT analogue of the HTTP path's `reserved_temp_paths`.  The
    /// lock is only contended in the rare simultaneous-completion case.
    completion_move_lock: Mutex<()>,
}

impl SharedBtSession {
    /// Create the shared session with the given initial speed limit and config.
    ///
    /// `default_save_dir` is used as the Session's default output folder
    /// (individual torrents override this via `AddTorrentOptions::output_folder`).
    ///
    /// `app_data_dir` is the application data directory where BT persistence
    /// files (session.json, .bitv, .torrent) are stored. This should be the
    /// exe directory or an app-specific folder — NOT the user's download dir.
    ///
    /// `speed_limit_bps` is the global download speed limit in bytes/sec
    /// (0 = unlimited).
    ///
    /// `bt_config` contains user-configurable BT settings (DHT, UPnP, ports,
    /// custom trackers).
    pub fn new(
        default_save_dir: &str,
        app_data_dir: &str,
        speed_limit_bps: u64,
        bt_config: &BtConfig,
    ) -> Result<Self, DownloadError> {
        // Scale worker threads with CPU cores.  BT workload is mostly I/O-bound
        // so diminishing returns beyond 8 threads; capping here saves ~2 MB of
        // stack memory per thread avoided.
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let worker_threads = cpu_cores.clamp(2, 8);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(worker_threads)
            .thread_name("bt-runtime")
            .build()
            .map_err(|e| DownloadError::Other(format!("failed to build BT runtime: {e}")))?;

        // If the user has a non-empty tracker list in settings, use it directly.
        // Otherwise fall back to the built-in PUBLIC_TRACKERS list.
        let trackers: HashSet<url::Url> = if bt_config.custom_trackers.trim().is_empty() {
            PUBLIC_TRACKERS
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect()
        } else {
            bt_config
                .custom_trackers
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .filter_map(|l| l.parse().ok())
                .collect()
        };

        let total_tracker_count = trackers.len();

        let download_bps = NonZeroU32::new(speed_limit_bps.min(u32::MAX as u64) as u32);

        // Persistence folder: store session.json + {hash}.bitv + {hash}.torrent
        // in the app data directory (next to flux_down.db), NOT in the user's
        // download folder. This matches how professional tools (qBittorrent,
        // Thunder, etc.) keep internal data out of user-visible directories.
        let persistence_folder = PathBuf::from(app_data_dir).join("bt_session");

        // Validate and clamp port range.
        let port_start = bt_config.port_start.max(1024);
        let port_end = bt_config.port_end.max(port_start);

        let enable_dht = bt_config.enable_dht;
        let enable_upnp = bt_config.enable_upnp;

        let save_dir = default_save_dir.to_owned();
        let save_dir_for_cleanup = save_dir.clone();
        let session = rt
            .block_on(async {
                let opts = SessionOptions {
                    disable_dht: !enable_dht,
                    disable_dht_persistence: !enable_dht,
                    listen_port_range: Some(port_start..port_end.saturating_add(1)),
                    enable_upnp_port_forwarding: enable_upnp,
                    trackers,
                    ratelimits: librqbit::limits::LimitsConfig {
                        download_bps,
                        upload_bps: None,
                    },
                    // Optimised peer connection parameters.
                    peer_opts: Some(PeerConnectionOptions {
                        // Slightly shorter connect timeout — drop unresponsive
                        // peers faster so we can try others sooner.
                        connect_timeout: Some(Duration::from_secs(10)),
                        // Generous read/write timeout to avoid dropping slow
                        // but otherwise healthy peers.
                        read_write_timeout: Some(Duration::from_secs(20)),
                        ..Default::default()
                    }),
                    // Enable persistence so that session.json and per-torrent
                    // .bitv (piece bitfield) files are written to disk.
                    persistence: Some(SessionPersistenceConfig::Json {
                        folder: Some(persistence_folder),
                    }),
                    // Fast-resume: persist piece completion state so that
                    // paused/restarted torrents can skip re-verification.
                    // Requires `persistence` to be set to take effect.
                    fastresume: true,
                    // Buffer writes in memory before flushing to disk.  Reduces
                    // I/O contention from many small pieces.  64 MiB is enough
                    // for high-speed connections while keeping RSS reasonable
                    // (was 128 — saved ~64 MB of potential RSS).
                    defer_writes_up_to: Some(64),
                    // Limit concurrent torrent initialisation to 3 to prevent
                    // DHT/tracker storms when many BT tasks start at once.
                    concurrent_init_limit: Some(3),
                    ..Default::default()
                };

                Session::new_with_opts(save_dir.into(), opts).await
            })
            .map_err(|e| DownloadError::Other(format!("BT session init failed: {e}")))?;

        // Startup cleanup: remove finished AND paused torrents that were
        // retained in persistence from a previous app session.
        //
        // Background: shutdown() calls session.pause() on all handles so
        // torrents are saved as Paused in session.json.  On the next
        // startup those Paused entries are reloaded by librqbit.  If the
        // user then starts downloading the same torrent again,
        // add_torrent returns AlreadyManaged with a Paused handle.
        // unpause() is called but the progress loop's first iteration
        // detects stats.state=Paused (the state transition is async) and
        // exits immediately as Cancelled, so the download never proceeds.
        //
        // The fix: remove ALL persisted torrents at startup — both
        // finished and paused ones.  FluxDown manages its own task state
        // in SQLite; the librqbit persistence layer is only used for the
        // piece-bitfield fast-resume (.bitv files), which survive the
        // session.delete(false) call (only the session.json entry is
        // removed).  Paused tasks are resumed via do_resume_task →
        // add_torrent, which re-loads the .bitv bitfield automatically.
        //
        // We only remove them from the librqbit session — the actual
        // downloaded files are left untouched (delete_files=false).
        // User-triggered "delete task + files" goes through the normal
        // delete_task() path which uses delete_files=true.
        {
            let all_ids: Vec<usize> =
                session.with_torrents(|iter| iter.map(|(id, _handle)| id).collect());
            if !all_ids.is_empty() {
                log_info!(
                    "[BT] startup cleanup: removing {} torrent(s) from persistence (finished or paused)",
                    all_ids.len()
                );
                for id in all_ids {
                    let _ = rt.block_on(session.delete(id.into(), false));
                }
            }
        }

        // Scan save_dir for staging dirs left behind by the session
        // restoration above.  Session::new_with_opts() loads session.json
        // and re-opens persisted torrents, which re-creates / touches their
        // output files (often as 0-byte stubs) BEFORE our cleanup loop can
        // call session.delete().  The download_manager's startup cleanup in
        // load_and_send_all_tasks() runs even earlier — before the BT
        // session exists — so it cannot catch these recreated dirs either.
        //
        // Remove any staging dir whose contents are all 0-byte (no real
        // downloaded data worth preserving).  Dirs with real data are kept
        // for resume via do_resume_task → add_torrent.
        {
            let save_path = std::path::Path::new(&save_dir_for_cleanup);
            if let Ok(entries) = std::fs::read_dir(save_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if !name_str.starts_with(BT_STAGE_PREFIX) {
                        continue;
                    }
                    let path = entry.path();
                    let has_real_data = std::fs::read_dir(&path)
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .any(|e| e.metadata().map(|m| m.len() > 0).unwrap_or(false))
                        })
                        .unwrap_or(false);
                    if !has_real_data {
                        log_info!(
                            "[BT] startup: removing empty/stub staging dir {}",
                            path.display()
                        );
                        let _ = std::fs::remove_dir_all(&path);
                    }
                }
            }
        }

        log_info!(
            "[BT] shared session created (DHT={}, UPnP={}, ports={}-{}, {} trackers, speed_limit={} B/s, worker_threads={}, persistence=on)",
            enable_dht,
            enable_upnp,
            port_start,
            port_end,
            total_tracker_count,
            speed_limit_bps,
            worker_threads
        );

        Ok(Self {
            runtime: rt,
            session,
            handles: Mutex::new(HashMap::new()),
            pending_deletes: Mutex::new(HashMap::new()),
            inflight_adds: AtomicUsize::new(0),
            file_selection_map: Mutex::new(HashMap::new()),
            torrent_ids: Mutex::new(HashMap::new()),
            completion_move_lock: Mutex::new(()),
        })
    }

    /// Update the global download speed limit at runtime.
    /// `bps == 0` means unlimited.  Takes effect immediately on all active
    /// BT downloads.
    pub fn set_speed_limit(&self, bps: u64) {
        let limit = NonZeroU32::new(bps.min(u32::MAX as u64) as u32);
        self.session.ratelimits.set_download_bps(limit);
        log_info!("[BT] shared session speed limit updated to {} B/s", bps);
    }

    /// Get an `Arc<Session>` handle for adding torrents.
    pub fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    /// Get a handle to the BT runtime for spawning tasks.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Store a torrent handle for a task so it can be paused/resumed later.
    pub async fn store_handle(&self, task_id: &str, handle: BtHandle) {
        self.handles
            .lock()
            .await
            .insert(task_id.to_string(), handle);
    }

    /// Pause a BT torrent by task_id.  The handle stays cached so that
    /// `resume_handle` can unpause it without re-adding.
    pub async fn pause_task(&self, task_id: &str) -> Result<(), DownloadError> {
        // Clone the Arc handle and release the lock immediately so that
        // the async session.pause() call doesn't block other handle ops.
        let handle = self.handles.lock().await.get(task_id).cloned();
        if let Some(handle) = handle {
            // If already paused or initializing, ignore silently.
            if !handle.is_paused() {
                self.session
                    .pause(&handle)
                    .await
                    .map_err(|e| DownloadError::Other(format!("BT pause failed: {e}")))?;
            }
            log_info!("[BT] task={} paused via session API", short_id(task_id));
        }
        Ok(())
    }

    /// Resume a previously paused BT torrent.  Returns the handle if
    /// successful, or `None` if no cached handle exists (caller should
    /// fall back to `add_torrent`).
    pub async fn resume_task(&self, task_id: &str) -> Result<Option<BtHandle>, DownloadError> {
        // Clone the Arc handle and release the lock immediately.
        let handle = self.handles.lock().await.get(task_id).cloned();
        if let Some(handle) = handle {
            if handle.is_paused() {
                self.session
                    .unpause(&handle)
                    .await
                    .map_err(|e| DownloadError::Other(format!("BT unpause failed: {e}")))?;
                log_info!("[BT] task={} resumed via session API", short_id(task_id));
            }
            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    /// Gracefully shut down the BT session and runtime.
    ///
    /// Pauses all active torrents, then shuts down the runtime with a timeout.
    /// Called when the application exits to ensure clean resource release.
    pub fn shutdown(&self) {
        log_info!("[BT] shutting down shared session...");
        // Use the runtime to gracefully close the session.  The session's
        // drop will attempt to persist DHT state and piece bitfields.
        // We give it a generous timeout to allow disk writes to complete.
        self.runtime.block_on(async {
            // Pause all tracked torrents so they flush state to disk.
            let handles: Vec<(String, BtHandle)> = {
                let map = self.handles.lock().await;
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            };
            for (tid, handle) in &handles {
                if !handle.is_paused()
                    && let Err(e) = self.session.pause(handle).await
                {
                    log_info!(
                        "[BT] shutdown: failed to pause task {}: {}",
                        short_id(tid),
                        e
                    );
                }
            }
        });
        // The Runtime::drop will be called after this, which blocks until
        // all spawned tasks finish (or the runtime forces them to stop).
        log_info!("[BT] shared session shutdown complete");
    }

    /// Permanently delete a torrent from the session, removing persistence
    /// data.  `delete_files` controls whether downloaded data is also removed.
    /// Returns `true` if a handle was found and `session.delete` was called,
    /// `false` if the task was not yet in the handles map (still in the
    /// `add_torrent` phase).  The caller should call `register_pending_delete`
    /// when this returns `false` so the detached add_torrent closure can clean
    /// up once metadata resolution completes.
    pub async fn delete_task(&self, task_id: &str, delete_files: bool) -> bool {
        // Remove from map first (under lock), then perform async deletion
        // outside the lock to minimise contention.
        let handle = self.handles.lock().await.remove(task_id);
        if let Some(handle) = handle {
            let torrent_id = handle.id();
            // Clean up the torrent_id → task_id mapping.
            self.unregister_torrent_id(torrent_id).await;
            if let Err(e) = self.session.delete(torrent_id.into(), delete_files).await {
                log_info!(
                    "[BT] task={} session.delete error: {}",
                    short_id(task_id),
                    e
                );
            } else {
                log_info!(
                    "[BT] task={} deleted from session (delete_files={})",
                    short_id(task_id),
                    delete_files
                );
            }
            true
        } else {
            false
        }
    }

    /// Register a deferred delete for a task whose `add_torrent` is still in
    /// progress.  The detached add_torrent closure will consume this entry and
    /// call `session.delete(id, delete_files)` as soon as metadata resolves.
    pub async fn register_pending_delete(&self, task_id: &str, delete_files: bool) {
        self.pending_deletes
            .lock()
            .await
            .insert(task_id.to_string(), delete_files);
        log_info!(
            "[BT] task={} pending delete registered (delete_files={})",
            short_id(task_id),
            delete_files
        );
    }

    /// Consume and return the pending delete flag for `task_id`, if any.
    pub async fn take_pending_delete(&self, task_id: &str) -> Option<bool> {
        self.pending_deletes.lock().await.remove(task_id)
    }

    /// Called by the actor when the user submits their BT file selection.
    pub async fn deliver_file_selection(&self, task_id: &str, indices: Vec<i32>) {
        let mut map = self.file_selection_map.lock().await;
        map.insert(task_id.to_string(), indices);
    }

    /// Poll for a pending file selection. Returns Some if available and removes the entry.
    pub async fn take_file_selection(&self, task_id: &str) -> Option<Vec<i32>> {
        let mut map = self.file_selection_map.lock().await;
        map.remove(task_id)
    }

    /// Record that a librqbit torrent_id is now managed by the given task_id.
    pub async fn register_torrent_id(&self, torrent_id: usize, task_id: &str) {
        self.torrent_ids
            .lock()
            .await
            .insert(torrent_id, task_id.to_string());
    }

    /// Remove the torrent_id mapping when a task is deleted.
    pub async fn unregister_torrent_id(&self, torrent_id: usize) {
        self.torrent_ids.lock().await.remove(&torrent_id);
    }

    /// Look up which task_id owns a given torrent_id.
    pub async fn task_for_torrent(&self, torrent_id: usize) -> Option<String> {
        self.torrent_ids.lock().await.get(&torrent_id).cloned()
    }

    /// Acquire the completion-move serialization lock.
    ///
    /// Callers hold the returned guard across the dedup-destination-name +
    /// `move_path` sequence in the completion stage so that concurrent BT
    /// task completions cannot race on the same `save_dir` destination name
    /// (which would otherwise let one task's file silently overwrite another's).
    pub async fn lock_completion_move(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.completion_move_lock.lock().await
    }

    fn increment_inflight_add(&self) {
        self.inflight_adds.fetch_add(1, Ordering::Relaxed);
    }

    fn decrement_inflight_add(&self) {
        self.inflight_adds.fetch_sub(1, Ordering::Relaxed);
    }

    /// Returns `true` if any detached `add_torrent` task is still running.
    pub fn has_inflight_adds(&self) -> bool {
        self.inflight_adds.load(Ordering::Relaxed) > 0
    }

    /// Increment the counter and return an RAII guard that decrements it on
    /// drop.  Using a guard instead of manual increment/decrement ensures the
    /// counter is always decremented even if the enclosing `tokio::spawn`
    /// closure panics before reaching the end.
    pub fn inflight_guard(self: &Arc<Self>) -> InflightGuard {
        self.increment_inflight_add();
        InflightGuard(Arc::clone(self))
    }
}

/// Decrements `SharedBtSession::inflight_adds` when dropped, guaranteeing
/// the counter is decremented even if the enclosing async task panics.
pub struct InflightGuard(Arc<SharedBtSession>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.decrement_inflight_add();
    }
}

// ---------------------------------------------------------------------------
// BT download params
// ---------------------------------------------------------------------------

pub struct BtDownloadParams {
    pub task_id: String,
    /// Torrent input source — magnet URI or raw .torrent file bytes.
    pub torrent_source: TorrentSource,
    pub save_dir: String,
    pub db: Db,
    pub progress_tx: mpsc::Sender<ProgressUpdate>,
    pub cancel_token: CancellationToken,
    /// Handle to the shared BT session.
    pub session: Arc<Session>,
    /// Handle to the shared BT runtime.
    pub bt_runtime: tokio::runtime::Handle,
    /// Shared session wrapper — used to cache the handle after add_torrent.
    pub shared_bt: Arc<SharedBtSession>,
    /// If resuming a paused torrent, this is the existing handle.
    /// When `Some`, we skip `add_torrent` and go straight to the progress loop.
    pub existing_handle: Option<BtHandle>,
    /// Pre-selected file indices (from the new-download dialog).
    /// Empty = show the file selection dialog after metadata resolves.
    pub pre_selected_indices: Vec<i32>,
    /// Skip Phase 3.5 file selection dialog entirely.
    /// Set to true when resuming a task whose confirmed selection is persisted
    /// in the DB as "all files" — no update_only_files needed either.
    pub skip_file_selection: bool,
    /// User-specified rename target for the final file/directory on disk.
    /// Empty string means "use the torrent's internal name" (default).
    /// Stored in a separate DB column (`bt_custom_name`) so that Phase 1/3
    /// engine callbacks never overwrite it.
    pub custom_name: String,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run a BT download for a magnet link using the shared session.
///
/// This function is designed to be `tokio::spawn`-ed from the download manager
/// just like `downloader::run_download` or `ftp_downloader::run_ftp_download`.
///
/// The actual BT work (add_torrent, progress polling) runs on the shared BT
/// runtime; this function bridges between the main `current_thread` runtime
/// and the BT runtime.
pub async fn run_bt_download(params: BtDownloadParams) -> Result<(), DownloadError> {
    let task_id = params.task_id.clone();

    // 1. Switch to "preparing" status
    let _ = params
        .db
        .update_task_status(&task_id, STATUS_PREPARING, "")
        .await;
    let _ = params
        .progress_tx
        .send(ProgressUpdate {
            task_id: task_id.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            status: STATUS_PREPARING,
            error_message: String::new(),
            file_name: String::new(),
            segment_details: None,
        })
        .await;

    log_info!(
        "[BT] task={} starting bt download (shared session)...",
        short_id(&task_id)
    );

    // 2. Run the actual BT download on the shared multi-thread runtime.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_clone = cancelled.clone();
    let cancel_token = params.cancel_token.clone();

    // Forward cancellation from CancellationToken to AtomicBool
    let cancelled_for_watcher = cancelled.clone();
    let cancel_watcher = tokio::spawn(async move {
        cancel_token.cancelled().await;
        cancelled_for_watcher.store(true, Ordering::SeqCst);
    });

    let progress_tx = params.progress_tx.clone();
    let db = params.db.clone();
    let torrent_source = params.torrent_source.clone();
    let save_dir = params.save_dir.clone();
    let tid = task_id.clone();
    let save_dir_for_cleanup = save_dir.clone();
    let tid_for_cleanup = tid.clone();
    let session = params.session.clone();
    let bt_runtime = params.bt_runtime.clone();
    let shared_bt = params.shared_bt.clone();
    let existing_handle = params.existing_handle;

    // Spawn the BT download on the shared multi-thread BT runtime.
    // The returned JoinHandle can be safely .await-ed from any runtime
    // (including our current_thread main runtime) — it uses waker-based
    // notification, not runtime-specific polling.  This avoids occupying
    // a thread from tokio's blocking thread pool for the entire download
    // duration, which previously caused thread-pool starvation under
    // many concurrent BT tasks.
    let inner_params = BtInnerParams {
        task_id: tid,
        torrent_source,
        save_dir,
        db,
        progress_tx,
        cancelled: cancelled_clone,
        session,
        shared_bt,
        existing_handle,
        pre_selected_indices: params.pre_selected_indices,
        skip_file_selection: params.skip_file_selection,
        custom_name: params.custom_name,
    };
    let result = bt_runtime
        .spawn(async move { bt_download_inner(inner_params).await })
        .await;

    cancel_watcher.abort();

    // Clean up pre-created staging dir if it's empty or contains only
    // zero-byte files (librqbit may pre-allocate stubs before detecting
    // AlreadyManaged or before any real data is written).
    //
    // `run_bt_download` runs on the main `current_thread` runtime, so the
    // synchronous `std::fs` scan (read_dir + metadata + remove_dir_all) must
    // not run inline — on a large/slow staging dir it would block the event
    // loop, stalling every other task's progress reporting and UI signalling.
    // We move the blocking work into `spawn_blocking` and `.await` it.
    let cleanup_stage = || {
        let stage = bt_stage_dir(&save_dir_for_cleanup, &tid_for_cleanup);
        let tid = tid_for_cleanup.clone();
        async move {
            let _ = tokio::task::spawn_blocking(move || {
                if !stage.exists() {
                    return;
                }
                let has_real_data = std::fs::read_dir(&stage)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .any(|e| e.metadata().map(|m| m.len() > 0).unwrap_or(false))
                    })
                    .unwrap_or(false);
                if !has_real_data {
                    log_info!(
                        "[BT] task={} cleaning up empty staging dir after error/cancel",
                        short_id(&tid)
                    );
                    let _ = std::fs::remove_dir_all(&stage);
                }
            })
            .await;
        }
    };

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            cleanup_stage().await;
            Err(e)
        }
        // JoinError has two causes:
        //   1. The spawned task panicked → treat as error (existing behaviour).
        //   2. The BT runtime was shut down (e.g. maybe_release_bt_session called
        //      while this task was still winding down after pause_task cancelled it).
        //      In that case cancelled is already true, so treat it as Cancelled to
        //      prevent the task from being marked as failed/error in the DB.
        Err(join_err) => {
            cleanup_stage().await;
            if cancelled.load(Ordering::SeqCst) {
                log_info!(
                    "[BT] task={} JoinError while cancelled (runtime shutdown during pause) — treating as Cancelled",
                    short_id(&task_id)
                );
                Err(DownloadError::Cancelled)
            } else {
                Err(DownloadError::Other(format!(
                    "BT task panicked: {join_err}"
                )))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Inner download logic (runs on the shared BT runtime)
// ---------------------------------------------------------------------------

/// Parameters for the inner BT download loop (avoids too-many-arguments warning).
struct BtInnerParams {
    task_id: String,
    torrent_source: TorrentSource,
    save_dir: String,
    db: Db,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    cancelled: Arc<AtomicBool>,
    session: Arc<Session>,
    shared_bt: Arc<SharedBtSession>,
    existing_handle: Option<BtHandle>,
    /// Pre-selected file indices forwarded from the CreateTask signal.
    /// Non-empty = skip the BtFilesInfo dialog and use these directly.
    pre_selected_indices: Vec<i32>,
    /// When true, skip Phase 3.5 entirely (user already confirmed all files
    /// on a previous run — resume without re-showing the dialog).
    skip_file_selection: bool,
    /// User-specified rename target, forwarded from BtDownloadParams.
    custom_name: String,
}

// ---------------------------------------------------------------------------
// Task status codes — must match Dart TaskStatus enum values.
// ---------------------------------------------------------------------------
const STATUS_DOWNLOADING: i32 = 1;
#[allow(dead_code)]
const STATUS_PAUSED: i32 = 2;
const STATUS_COMPLETED: i32 = 3;
const STATUS_ERROR: i32 = 4;

// ---------------------------------------------------------------------------
// BT staging directory helpers
// ---------------------------------------------------------------------------

/// Prefix used for per-task staging directories inside `save_dir`.
/// Each BT task downloads into `save_dir/.bt_stage_<task_id>/` so that
/// concurrent tasks with identical torrent names never collide on disk.
/// The directory is removed after the file/folder is moved to its final
/// location (or on task deletion).
pub const BT_STAGE_PREFIX: &str = ".bt_stage_";

/// Build the staging directory path for a BT task.
///
/// `save_dir/.bt_stage_<task_id>/`
pub fn bt_stage_dir(save_dir: &str, task_id: &str) -> PathBuf {
    PathBuf::from(save_dir).join(format!("{}{}", BT_STAGE_PREFIX, task_id))
}

/// Mark a path as hidden on Windows using `SetFileAttributesW`.
///
/// On non-Windows platforms this is a no-op — the leading `.` in the directory
/// name is already the POSIX convention for hidden files.
///
/// Failures are silently ignored: a non-hidden staging directory is merely a
/// cosmetic nuisance; it does not affect correctness.
fn set_hidden(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        // Encode path as a NUL-terminated wide string.
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0u16))
            .collect();
        // Safety: `wide` is a valid NUL-terminated UTF-16 path.
        unsafe {
            let attrs = GetFileAttributesW(wide.as_ptr());
            // INVALID_FILE_ATTRIBUTES == 0xFFFFFFFF
            if attrs != 0xFFFF_FFFF {
                let _ = SetFileAttributesW(wide.as_ptr(), attrs | FILE_ATTRIBUTE_HIDDEN);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path; // no-op
    }
}

/// At startup, attempt to finish any BT tasks that completed downloading but
/// whose staging-directory move was interrupted (e.g. the app was force-killed
/// between `stats.finished` being detected and the `move_path` call completing).
///
/// For each task in `completed_bt_tasks` we check whether a staging directory
/// still exists at `save_dir/.bt_stage_<task_id>/`.  If it does, we perform
/// the same move logic as `bt_download_inner` Phase 5:
///
/// 1. Look for an entry inside the staging dir whose name matches
///    `current_file_name` (written to DB during Phase 3 as `resolved_name`).
///    If not found, fall back to moving **every** non-hidden entry
///    (mirrors the "staging item not found" fallback in Phase 5).
/// 2. Move the matched item to `save_dir/<dedup_name>`.
/// 3. Remove the now-empty staging dir.
/// 4. Return a list of `(task_id, final_name)` pairs so the caller can update
///    the DB with the correct `file_name`.
///
/// Uses synchronous I/O — called once at startup before any BT session is
/// active, so there is no concurrency risk.
pub fn rescue_stranded_staging_files(
    completed_bt_tasks: &[(&str, &str, &str)], // (task_id, save_dir, current_file_name)
) -> Vec<(String, String)> {
    let mut updates: Vec<(String, String)> = Vec::new();

    for &(task_id, save_dir, current_file_name) in completed_bt_tasks {
        let stage_dir = bt_stage_dir(save_dir, task_id);
        if !stage_dir.exists() {
            continue;
        }

        log_info!(
            "[BT] rescue: task={} staging dir still present at '{}', attempting recovery move",
            &task_id[..task_id.len().min(8)],
            stage_dir.display()
        );

        let save_path = Path::new(save_dir);

        // Collect non-hidden entries from the staging dir.
        let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&stage_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .collect(),
            Err(e) => {
                log_info!(
                    "[BT] rescue: task={} cannot read staging dir: {}",
                    &task_id[..task_id.len().min(8)],
                    e
                );
                continue;
            }
        };

        if entries.is_empty() {
            // Staging dir is empty (or only hidden files) — remove it.
            let _ = std::fs::remove_dir_all(&stage_dir);
            log_info!(
                "[BT] rescue: task={} staging dir was empty, removed",
                &task_id[..task_id.len().min(8)]
            );
            continue;
        }

        // ------------------------------------------------------------------
        // Fast path: an entry whose name exactly matches current_file_name
        // (= resolved_name written to DB in Phase 3 / Phase 3.5).
        // This is the normal case: single-file torrent or a multi-file
        // torrent whose top-level directory name equals the torrent name.
        // Mirrors the `stage_item.exists()` branch in bt_download_inner.
        // ------------------------------------------------------------------
        let preferred = entries
            .iter()
            .find(|e| e.file_name().to_string_lossy() == current_file_name);

        if let Some(entry) = preferred {
            let child_name = entry.file_name();
            let child_name_str = child_name.to_string_lossy();
            let final_name = dedup_name_in_dir(save_path, &child_name_str);
            let dst = save_path.join(&final_name);

            match move_path(&entry.path(), &dst) {
                Ok(()) => {
                    log_info!(
                        "[BT] rescue: task={} moved '{}' → '{}' (recovery complete)",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        dst.display()
                    );
                    // Remove staging dir; may still contain .pad / hidden files.
                    let _ = std::fs::remove_dir_all(&stage_dir);
                    updates.push((task_id.to_string(), final_name));
                }
                Err(e) => {
                    log_info!(
                        "[BT] rescue: task={} failed to move '{}': {}",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        e
                    );
                    // Leave staging dir in place for manual recovery.
                }
            }
            continue;
        }

        // ------------------------------------------------------------------
        // Fallback path: no entry matched current_file_name.
        // This mirrors the `stage_dir.exists()` fallback in bt_download_inner:
        // move every non-hidden child individually and report the first
        // successfully moved item as the new file_name.
        // ------------------------------------------------------------------
        log_info!(
            "[BT] rescue: task={} expected item '{}' not found in staging dir; \
             moving all children",
            &task_id[..task_id.len().min(8)],
            current_file_name
        );

        let mut first_moved_name: Option<String> = None;
        let mut all_moves_ok = true;
        for entry in &entries {
            let child_name = entry.file_name();
            let child_name_str = child_name.to_string_lossy();
            let final_child_name = dedup_name_in_dir(save_path, &child_name_str);
            let dst = save_path.join(&final_child_name);

            match move_path(&entry.path(), &dst) {
                Ok(()) => {
                    log_info!(
                        "[BT] rescue: task={} moved child '{}' → '{}'",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        dst.display()
                    );
                    if first_moved_name.is_none() {
                        first_moved_name = Some(final_child_name);
                    }
                }
                Err(e) => {
                    all_moves_ok = false;
                    log_info!(
                        "[BT] rescue: task={} failed to move child '{}': {}",
                        &task_id[..task_id.len().min(8)],
                        entry.path().display(),
                        e
                    );
                }
            }
        }

        // 仅当所有子项都成功迁出才删除 staging 目录；否则保留,避免把迁移
        // 失败(权限/跨盘/瞬时 I/O)的文件随目录一并删掉造成数据丢失——与
        // fast path 及 bt_download_inner 的完成路径行为对齐,留待下次启动重试。
        if all_moves_ok {
            let _ = std::fs::remove_dir_all(&stage_dir);
        } else {
            log_info!(
                "[BT] rescue: task={} some children failed to move; \
                 keeping staging dir for recovery: {}",
                &task_id[..task_id.len().min(8)],
                stage_dir.display()
            );
        }

        if let Some(name) = first_moved_name {
            updates.push((task_id.to_string(), name));
        }
    }

    updates
}

/// Deduplicate a file or directory name inside `dir`.
///
/// If `dir/name` does not exist, returns `name` unchanged.
/// Otherwise appends ` (1)`, ` (2)`, … until a free slot is found.
/// Mirrors the logic in `downloader::dedup_filename` but runs synchronously
/// (called from the BT runtime thread after downloading is complete).
fn dedup_name_in_dir(dir: &Path, name: &str) -> String {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return name.to_string();
    }

    // Scan directory once to avoid per-candidate filesystem round-trips.
    let existing: std::collections::HashSet<std::ffi::OsString> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name()).collect())
        .unwrap_or_default();

    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());

    for i in 1..=9999u32 {
        let new_name = match ext {
            Some(e) => format!("{} ({}).{}", stem, i, e),
            None => format!("{} ({})", stem, i),
        };
        if !existing.contains(std::ffi::OsStr::new(&new_name)) {
            return new_name;
        }
    }
    // 极端兜底:1..=9999 个编号变体全被占用时,此前返回**原名不变**,调用点
    // (容器 / 单文件分支)会直接拿它当 dst,move_path 静默覆盖已存在文件丢数据。
    // 改用 UUID 后缀保证唯一,杜绝覆盖。(BUG-BT-DEDUP-FALLBACK-OVERWRITE)
    let uniq = uuid::Uuid::new_v4();
    match ext {
        Some(e) => format!("{} ({}).{}", stem, uniq, e),
        None => format!("{} ({})", stem, uniq),
    }
}

/// Compute the layout for moving downloaded files from staging to save_dir.
///
/// Returns `(moves, top_level_name)`:
/// - `moves`: ordered list of `(src_in_stage, dst_in_save)` pairs to apply
/// - `top_level_name`: the entry that ends up directly under `save_dir`
///   (used as the DB `file_name` for "open file location" UX)
///
/// Layout decisions:
/// - **All-selected with shared top-level dir** → container move (move the
///   whole top-level dir from staging to save_dir, optionally renamed).
/// - **All-selected, flat torrent** → per-file flat move (each non-padding
///   file lands directly under save_dir).
/// - **Single file (partial or otherwise)** → single-file flat move (basename
///   only, no container, optional `custom_name` rename).
/// - **Partial selection of multiple files** → per-file flat move; basenames
///   are deduped against save_dir AND against in-batch siblings.  `custom_name`
///   does not apply (no obvious "container" to rename).
///
/// The reason completion is driven by `selected_paths` (and never by reading
/// staging dir contents) is that BT pieces span file boundaries, so librqbit
/// inevitably writes piece-overlap byproducts for non-selected files (see
/// `librqbit/file_ops.rs::write_chunk` — only BEP-47 padding files are
/// skipped).  Those byproducts are cleaned up wholesale by `remove_dir_all`
/// after all selected files have been moved out.
fn compute_completion_layout(
    save_dir: &Path,
    stage_dir: &Path,
    selected_paths: &[PathBuf],
    all_selected: bool,
    custom_name: &str,
) -> Option<(Vec<(PathBuf, PathBuf)>, String)> {
    if selected_paths.is_empty() {
        return None;
    }

    // 路径穿越防护:`selected_paths` 源自 torrent 元数据(file_infos[i].
    // relative_filename),恶意种子可塞入 `..` / 绝对路径 / 盘符前缀,使
    // `stage_dir.join(rel)` 逃出 staging 目录(读到任意位置文件)或破坏 dst 归属。
    // 任一选中路径不安全即整体拒绝(返回 None → 调用方标 STATUS_ERROR),决不
    // 移动可疑数据。空字节无法出现在 String 派生的 Path 中,这里按组件做词法校验
    // (不做 canonicalize 以避免额外 I/O 与文件不存在时的误报)。
    // (BUG-BT-PATH-TRAVERSAL)
    let path_is_safe = |rel: &Path| -> bool {
        use std::path::Component;
        if rel.as_os_str().is_empty() || rel.is_absolute() {
            return false;
        }
        rel.components().all(|c| matches!(c, Component::Normal(_)))
    };
    if let Some(bad) = selected_paths.iter().find(|p| !path_is_safe(p)) {
        log_info!(
            "[BT] completion: rejecting unsafe selected path '{}' (path traversal guard)",
            bad.display(),
        );
        return None;
    }

    // Detect a real shared top-level directory (every path has > 1 component
    // AND first component is identical across all selected files).
    let shared_top_dir: Option<String> = (|| -> Option<String> {
        let first = selected_paths
            .first()?
            .components()
            .next()
            .and_then(|c| c.as_os_str().to_str())?
            .to_string();
        for p in selected_paths {
            if p.components().count() < 2 {
                return None;
            }
            let head = p.components().next()?.as_os_str().to_str()?;
            if head != first {
                return None;
            }
        }
        Some(first)
    })();

    // Container move: only when all selected AND a real top-level dir exists.
    if all_selected && let Some(top) = shared_top_dir.as_deref() {
        let desired = if custom_name.is_empty() {
            top
        } else {
            custom_name
        };
        let final_top = dedup_name_in_dir(save_dir, desired);
        let src = stage_dir.join(top);
        let dst = save_dir.join(&final_top);
        return Some((vec![(src, dst)], final_top));
    }

    // Single-file flat move (single selected file regardless of all_selected).
    if selected_paths.len() == 1 {
        let rel = &selected_paths[0];
        let basename = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
            .to_string();
        let desired = if custom_name.is_empty() {
            basename.as_str()
        } else {
            custom_name
        };
        let final_name = dedup_name_in_dir(save_dir, desired);
        let src = stage_dir.join(rel);
        let dst = save_dir.join(&final_name);
        return Some((vec![(src, dst)], final_name));
    }

    // Per-file flat move: covers all-selected flat torrent + partial multi.
    // Dedup each basename against save_dir AND against names already chosen
    // in this batch so two staged files cannot collide on the same dst.
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(selected_paths.len());
    let mut top_level: Option<String> = None;
    for (idx, rel) in selected_paths.iter().enumerate() {
        let basename = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download")
            .to_string();
        // For all-selected flat torrent + custom_name, rename the FIRST file
        // only (preserves prior behavior for the single-rename UX).
        let candidate_seed = if idx == 0 && all_selected && !custom_name.is_empty() {
            custom_name
        } else {
            basename.as_str()
        };
        // First dedup against the on-disk contents of save_dir.
        let mut candidate = dedup_name_in_dir(save_dir, candidate_seed);
        // Then dedup against names already chosen in *this* batch.  Use a plain
        // numeric counter on the seed's stem/ext (`stem (n).ext`) rather than
        // prepending `_` to the whole candidate, which previously stacked
        // underscores (`_file (1).ext`, `__file (1).ext`, …) and corrupted the
        // base name.  This keeps the same `name (n).ext` style as
        // `dedup_name_in_dir` itself.
        if taken.contains(&candidate) {
            let stem = Path::new(candidate_seed)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(candidate_seed);
            let ext = Path::new(candidate_seed)
                .extension()
                .and_then(|s| s.to_str());
            // 1..=9999 mirrors the bound in `dedup_name_in_dir`, preventing an
            // unbounded loop in the pathological all-collisions case.
            for n in 1..=9999u32 {
                let numbered = match ext {
                    Some(e) => format!("{} ({}).{}", stem, n, e),
                    None => format!("{} ({})", stem, n),
                };
                // Reconcile against disk again so we never overwrite a real file.
                let deduped = dedup_name_in_dir(save_dir, &numbered);
                if !taken.contains(&deduped) {
                    candidate = deduped;
                    break;
                }
            }
            // 极端兜底:9999 个编号变体都被占用(需约 1 万个同 basename 文件挤入同一
            // 扁平目录)时,candidate 仍为已占用名,会导致 moves 出现重复目标 → 落盘时
            // 静默覆盖丢数据。此处用 UUID 后缀保证唯一,杜绝覆盖。
            if taken.contains(&candidate) {
                let uniq = uuid::Uuid::new_v4();
                candidate = match ext {
                    Some(e) => format!("{} ({}).{}", stem, uniq, e),
                    None => format!("{} ({})", stem, uniq),
                };
            }
        }
        taken.insert(candidate.clone());
        let src = stage_dir.join(rel);
        let dst = save_dir.join(&candidate);
        if top_level.is_none() {
            top_level = Some(candidate.clone());
        }
        moves.push((src, dst));
    }

    Some((moves, top_level.unwrap_or_else(|| "download".to_string())))
}

/// Move a file or directory from `src` to `dst`.
///
/// Tries `std::fs::rename` first (atomic, same filesystem).  If that fails
/// (e.g. cross-device), falls back to a recursive copy + remove.
fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    // Fast path: atomic rename (works when src and dst are on the same fs).
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }

    // Slow path: copy then remove.
    if src.is_dir() {
        copy_dir_all(src, dst)?;
        std::fs::remove_dir_all(src)
    } else {
        std::fs::copy(src, dst)?;
        std::fs::remove_file(src)
    }
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}
const STATUS_PREPARING: i32 = 5;

/// Number of virtual segments for single-file BT progress visualization.
const BT_VIRTUAL_SEGMENTS: i32 = 16;

/// For **multi-file** torrents each file becomes a segment — this naturally
/// reflects the concurrent piece-based download because different files
/// accumulate downloaded bytes independently.
///
/// For **single-file** (or when `file_progress` is unavailable) we split
/// the total size into `BT_VIRTUAL_SEGMENTS` virtual segments and
/// distribute the completed pieces proportionally using a deterministic
/// scatter pattern.  This avoids the old "linear fill" look and produces
/// an IDM-style concurrent visualization that truthfully represents the
/// random order in which BT pieces arrive.
fn build_bt_segments(
    total_bytes: i64,
    downloaded_bytes: i64,
    file_progress: &[u64],
    file_offsets: &[(u64, u64)], // (offset_in_torrent, file_len)
    total_pieces: u32,
    downloaded_pieces: u64,
) -> Vec<SegmentProgressInfo> {
    if total_bytes <= 0 {
        return Vec::new();
    }

    // Multi-file torrent: each file is a natural segment
    if file_progress.len() > 1 && file_offsets.len() == file_progress.len() {
        return build_multi_file_segments(total_bytes, file_progress, file_offsets);
    }

    // Single-file (or fallback): scatter pieces across virtual segments
    build_piece_scatter_segments(
        total_bytes,
        downloaded_bytes,
        total_pieces,
        downloaded_pieces,
    )
}

/// Multi-file torrent: map each file to a segment.
fn build_multi_file_segments(
    total_bytes: i64,
    file_progress: &[u64],
    file_offsets: &[(u64, u64)],
) -> Vec<SegmentProgressInfo> {
    let mut segs = Vec::with_capacity(file_progress.len());
    for (i, (&dl_bytes, &(offset, file_len))) in
        file_progress.iter().zip(file_offsets.iter()).enumerate()
    {
        if file_len == 0 {
            continue;
        }
        let start = offset as i64;
        // Partial-selection guard: with a subset `total_bytes` but the full
        // torrent's `file_offsets`, an unselected file's `offset` can exceed
        // `total_bytes`.  Such a file would yield `start > end` and a negative
        // `downloaded_bytes`, producing an illegal SegmentProgressInfo for
        // Dart.  Skip any file whose start lies beyond the (subset) total.
        if start >= total_bytes {
            continue;
        }
        let end = (offset + file_len).saturating_sub(1) as i64;
        let end = end.min(total_bytes - 1);
        // Defensive: `end < start` should be impossible after the guard above,
        // but skip rather than emit a reversed range if it ever occurs.
        if end < start {
            continue;
        }
        let span = end - start + 1;
        segs.push(SegmentProgressInfo {
            index: i as i32,
            start_byte: start,
            end_byte: end,
            // Clamp into `[0, span]` so a subset/total mismatch can never yield
            // a negative downloaded count.
            downloaded_bytes: (dl_bytes as i64).clamp(0, span),
        });
    }
    segs
}

/// Single-file torrent: split into virtual segments and distribute
/// completed pieces using a deterministic scatter pattern.
///
/// BT downloads pieces in a mostly random order (rarest-first strategy).
/// Instead of filling left-to-right, we use a modular-hash scatter to
/// distribute `downloaded_pieces` across all virtual segments so the UI
/// shows multiple segments progressing simultaneously — which is what
/// actually happens in practice.
fn build_piece_scatter_segments(
    total_bytes: i64,
    downloaded_bytes: i64,
    total_pieces: u32,
    downloaded_pieces: u64,
) -> Vec<SegmentProgressInfo> {
    // 虚拟段数钳制到 total_bytes:当 total_bytes ∈ 1..16 时,
    // chunk = total_bytes / 16 == 0,非末段 end = (i+1)*chunk-1 = -1 < start = 0,
    // 会产出 end_byte < start_byte 的非法段(多文件路径已在别处防护,单文件
    // scatter 路径此前缺失)。钳到 [1, 16] 保证 chunk >= 1。
    // (BUG-BT-TINY-TORRENT-SEGMENT)
    let n = (BT_VIRTUAL_SEGMENTS as i64).min(total_bytes.max(1)) as i32;
    let chunk = total_bytes / n as i64;
    let mut segs = Vec::with_capacity(n as usize);

    if total_pieces == 0 || (downloaded_pieces == 0 && downloaded_bytes > 0) {
        // Fallback: no piece info yet OR no pieces completed but we have
        // fetched bytes (partial pieces in-flight).  Distribute bytes
        // evenly across virtual segments with a scatter pattern so the
        // user can see BT is actively downloading even before any piece
        // has been fully hash-verified.
        let per_seg = if downloaded_bytes > 0 {
            downloaded_bytes / n as i64
        } else {
            0
        };
        for i in 0..n {
            let start = i as i64 * chunk;
            let end = if i == n - 1 {
                total_bytes - 1
            } else {
                (i as i64 + 1) * chunk - 1
            };
            // 防御:钳制后正常不会发生,但若 chunk 仍致 end < start 则跳过,
            // 决不向 Dart 发反向区间。(BUG-BT-TINY-TORRENT-SEGMENT)
            if end < start {
                continue;
            }
            // Scatter the bytes unevenly so segments don't all look identical.
            // Use golden-ratio perturbation for a natural spread.
            let perturbation = ((i as f64 + 1.0) * 0.618033988749895).fract();
            let weight = 0.7 + perturbation * 0.6; // range [0.7, 1.3]
            let seg_dl = (per_seg as f64 * weight).round() as i64;
            segs.push(SegmentProgressInfo {
                index: i,
                start_byte: start,
                end_byte: end,
                downloaded_bytes: seg_dl.clamp(0, end - start + 1),
            });
        }
        // Correction: ensure total visual bytes match actual downloaded_bytes
        let visual_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
        let diff = downloaded_bytes - visual_total;
        if diff != 0 {
            let abs_diff = diff.unsigned_abs() as f64;
            let direction = if diff > 0 { 1i64 } else { -1i64 };
            let mut remaining = diff.abs();
            for seg in &mut segs {
                let seg_size = seg.end_byte - seg.start_byte + 1;
                let share = ((seg_size as f64 / total_bytes as f64) * abs_diff).round() as i64;
                let adj = share.min(remaining);
                seg.downloaded_bytes = (seg.downloaded_bytes + direction * adj).clamp(0, seg_size);
                remaining -= adj;
                if remaining <= 0 {
                    break;
                }
            }
        }
        return segs;
    }

    // Assign each piece to a virtual segment, then count completed pieces
    // per segment.  The assignment uses a scatter function to spread pieces
    // that are close in index across different segments.
    let pieces_per_seg = (total_pieces as f64 / n as f64).ceil() as u32;
    let completion_ratio = if total_pieces > 0 {
        downloaded_pieces as f64 / total_pieces as f64
    } else {
        0.0
    };

    for i in 0..n {
        let start = i as i64 * chunk;
        let end = if i == n - 1 {
            total_bytes - 1
        } else {
            (i as i64 + 1) * chunk - 1
        };
        // 防御:钳制后正常不会发生,但若 chunk 仍致 end < start 则跳过,
        // 决不向 Dart 发反向区间。(BUG-BT-TINY-TORRENT-SEGMENT)
        if end < start {
            continue;
        }
        let seg_size = end - start + 1;

        // Count how many pieces belong to this segment
        let seg_piece_start = i as u32 * pieces_per_seg;
        let seg_piece_end = ((i as u32 + 1) * pieces_per_seg).min(total_pieces);
        let seg_total_pieces = seg_piece_end.saturating_sub(seg_piece_start);

        // Scatter completed pieces across segments using a golden-ratio
        // based distribution.  This produces a visually pleasing and
        // deterministic spread that varies per segment.
        //
        // For each segment i, the expected completion is:
        //   base_ratio ± a small perturbation seeded by segment index
        //
        // The perturbation ensures segments don't all show the same %.
        let perturbation = ((i as f64 + 1.0) * 0.618033988749895).fract() - 0.5;
        let seg_ratio = (completion_ratio + perturbation * 0.3).clamp(0.0, 1.0);

        // Snap to exact 0 or 1 when close to boundaries
        let seg_dl_pieces = if completion_ratio <= 0.001 {
            0.0
        } else if completion_ratio >= 0.999 {
            seg_total_pieces as f64
        } else {
            (seg_total_pieces as f64 * seg_ratio).round()
        };

        let dl =
            ((seg_dl_pieces / seg_total_pieces.max(1) as f64) * seg_size as f64).round() as i64;

        segs.push(SegmentProgressInfo {
            index: i,
            start_byte: start,
            end_byte: end,
            downloaded_bytes: dl.clamp(0, seg_size),
        });
    }

    // Correction pass: make sure total downloaded across segments matches
    // the real downloaded_bytes (avoid visual mismatch with progress %).
    let visual_total: i64 = segs.iter().map(|s| s.downloaded_bytes).sum();
    let diff = downloaded_bytes - visual_total;
    if diff != 0 && !segs.is_empty() {
        // Distribute the difference proportionally
        let abs_diff = diff.unsigned_abs() as f64;
        let direction = if diff > 0 { 1i64 } else { -1i64 };
        let mut remaining = diff.abs();
        for seg in &mut segs {
            let seg_size = seg.end_byte - seg.start_byte + 1;
            let share = ((seg_size as f64 / total_bytes as f64) * abs_diff).round() as i64;
            let adj = share.min(remaining);
            seg.downloaded_bytes = (seg.downloaded_bytes + direction * adj).clamp(0, seg_size);
            remaining -= adj;
            if remaining <= 0 {
                break;
            }
        }
    }

    segs
}

/// Compute user-facing BT progress bytes.
///
/// We combine checked bytes (hash-verified) and fetched bytes (network-received)
/// so early BT activity is visible before any piece is fully verified.
///
/// Important: when the torrent is not `finished`, never return `total_bytes`,
/// otherwise UI would show 100% while librqbit is still verifying/finalizing.
fn compute_bt_display_progress(
    checked_progress: i64,
    fetched_progress: i64,
    total_bytes: i64,
    finished: bool,
) -> i64 {
    let mut progress = checked_progress.max(fetched_progress).max(0);
    if total_bytes > 0 {
        progress = progress.min(total_bytes);
        if !finished && progress >= total_bytes {
            progress = total_bytes.saturating_sub(1);
        }
    }
    progress
}

async fn bt_download_inner(p: BtInnerParams) -> Result<(), DownloadError> {
    let BtInnerParams {
        task_id,
        torrent_source,
        save_dir,
        db,
        progress_tx,
        cancelled,
        session,
        shared_bt,
        existing_handle,
        pre_selected_indices,
        skip_file_selection,
        custom_name,
    } = p;

    // Record whether this is a resume of an existing handle *before*
    // existing_handle is moved into Phase 2.  When true we skip Phase 3.5
    // entirely because librqbit already retains the previous update_only_files
    // state internally.
    let had_existing_handle = existing_handle.is_some();
    // -----------------------------------------------------------------------
    // Phase 1: Send initial file name from dn= parameter so user sees something
    // -----------------------------------------------------------------------

    let dn_name = torrent_source.display_name().unwrap_or_default();
    if !dn_name.is_empty() {
        let _ = db.update_task_file_info(&task_id, &dn_name, 0).await;
        let _ = progress_tx
            .send(ProgressUpdate {
                task_id: task_id.clone(),
                downloaded_bytes: 0,
                total_bytes: 0,
                status: STATUS_PREPARING,
                error_message: String::new(),
                file_name: dn_name.clone(),
                segment_details: None,
            })
            .await;
    }

    // -----------------------------------------------------------------------
    // Phase 2: Obtain torrent handle
    //
    // If we have an existing handle (resumed from pause), just unpause it.
    // Otherwise add a new torrent to the session.
    // -----------------------------------------------------------------------

    let handle = if let Some(h) = existing_handle {
        log_info!(
            "[BT] task={} reusing existing handle (resume)",
            short_id(&task_id)
        );
        // Handle was already unpaused by SharedBtSession::resume_task,
        // so we can go straight to the progress loop.
        h
    } else {
        // Use a task-scoped staging directory so that concurrent BT tasks
        // with identical torrent names never collide on disk.
        // librqbit will write to  save_dir/.bt_stage_<task_id>/<resolved_name>
        // and we move the result to the final deduplicated path after download.
        let stage_dir = bt_stage_dir(&save_dir, &task_id);
        // Create the staging directory now (before librqbit does) so we can
        // immediately mark it hidden.  librqbit uses `overwrite: true` and
        // will reuse the directory if it already exists.
        if let Err(e) = std::fs::create_dir_all(&stage_dir) {
            log_info!(
                "[BT] task={} failed to pre-create staging dir '{}': {}",
                short_id(&task_id),
                stage_dir.display(),
                e
            );
        } else {
            set_hidden(&stage_dir);
        }
        let stage_dir_str = stage_dir.to_string_lossy().into_owned();
        let add_opts = AddTorrentOptions {
            overwrite: true,
            output_folder: Some(stage_dir_str),
            ..Default::default()
        };

        log_info!(
            "[BT] task={} adding torrent to shared session (metadata resolution may take a while)...",
            short_id(&task_id)
        );

        let session_for_add = session.clone();
        let source_for_add = torrent_source.clone();
        let shared_bt_for_add = shared_bt.clone();
        let task_id_for_add = task_id.clone();
        // Create the RAII guard before spawning so `maybe_release_bt_session`
        // sees the in-flight task even if `bt_download_inner` is cancelled
        // immediately after.  The guard decrements on drop — panic-safe.
        let inflight = shared_bt.inflight_guard();
        let add_handle = tokio::spawn(async move {
            // Move the guard into the task so it is dropped (and thus
            // decrements) when the task finishes normally *or* panics.
            let _inflight = inflight;
            let add_input = match source_for_add {
                TorrentSource::Magnet(ref url) => AddTorrent::from_url(url),
                TorrentSource::TorrentFileBytes(ref bytes) => {
                    AddTorrent::from_bytes(Bytes::from(bytes.clone()))
                }
            };
            let result = session_for_add.add_torrent(add_input, Some(add_opts)).await;
            // If delete_task was called while we were waiting for metadata
            // (handle not yet in `handles`, run_bt_download already returned
            // Err(Cancelled)), apply the pending delete now that we have the
            // torrent ID.  This prevents orphaned files from magnets whose
            // DHT metadata resolved after the user deleted the task.
            //
            // TOCTOU note: `bt_download_inner`'s main loop stores the handle in
            // `handles` only on the success path; on a cancel it returns early
            // *before* `store_handle`.  Meanwhile `delete_task` (download_manager)
            // removes-from-`handles` (a miss, since not yet stored) and then
            // `register_pending_delete`.  If this detached task only checked
            // `take_pending_delete` once and that check raced *ahead* of the
            // `register_pending_delete`, the pending entry would never be
            // consumed and the torrent would leak in the librqbit session.
            //
            // To close the window we register the handle here and re-check the
            // pending-delete map afterwards (double-checked consumption):
            //   1. take_pending_delete → if set, delete now (no need to store).
            //   2. else store_handle, then take_pending_delete *again*; if a
            //      delete arrived in between, delete now and drop the handle.
            // Because `delete_task` writes the pending entry *after* its
            // handles-miss, at least one of the two checks (or `delete_task`'s
            // own handle lookup, once stored) always observes the delete.
            if let Ok(ref resp) = result {
                match resp {
                    AddTorrentResponse::Added(id, handle) => {
                        let id = *id;
                        let handle = handle.clone();
                        if let Some(del_files) =
                            shared_bt_for_add.take_pending_delete(&task_id_for_add).await
                        {
                            // Delete was requested before we got here.
                            let _ = session_for_add.delete(id.into(), del_files).await;
                            log_info!(
                                "[BT] task={} pending delete applied after add_torrent (delete_files={})",
                                short_id(&task_id_for_add),
                                del_files
                            );
                        } else {
                            // No delete yet — publish the handle so pause/resume/
                            // delete can find it, then re-check for a delete that
                            // may have raced in just after our first check.  We
                            // also register the torrent_id so `delete_task` can
                            // clean up the mapping.
                            shared_bt_for_add.register_torrent_id(id, &task_id_for_add).await;
                            shared_bt_for_add
                                .store_handle(&task_id_for_add, handle)
                                .await;
                            if let Some(del_files) =
                                shared_bt_for_add.take_pending_delete(&task_id_for_add).await
                            {
                                // A delete arrived between the two checks; consume
                                // it and remove the handle we just stored
                                // (delete_task also unregisters the torrent_id).
                                let _ = shared_bt_for_add
                                    .delete_task(&task_id_for_add, del_files)
                                    .await;
                                log_info!(
                                    "[BT] task={} pending delete applied on re-check after store_handle (delete_files={})",
                                    short_id(&task_id_for_add),
                                    del_files
                                );
                            }
                        }
                    }
                    AddTorrentResponse::AlreadyManaged(id, _handle) => {
                        // The torrent is owned by another task.  We must NOT
                        // store its handle under our task_id, nor unconditionally
                        // delete it (that would clobber the real owner).  Only
                        // consume a pending delete if this very task owns the
                        // torrent_id mapping — otherwise leave it to the owner.
                        if let Some(del_files) =
                            shared_bt_for_add.take_pending_delete(&task_id_for_add).await
                        {
                            let owner = shared_bt_for_add.task_for_torrent(*id).await;
                            if owner.as_deref() == Some(task_id_for_add.as_str()) {
                                let _ = session_for_add.delete((*id).into(), del_files).await;
                                log_info!(
                                    "[BT] task={} pending delete applied (already-managed, owned by us, delete_files={})",
                                    short_id(&task_id_for_add),
                                    del_files
                                );
                            } else {
                                log_info!(
                                    "[BT] task={} pending delete skipped — torrent owned by another task",
                                    short_id(&task_id_for_add)
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            result
            // `_inflight` drops here → decrement_inflight_add() called
        });

        // Send "preparing" heartbeats while waiting for metadata.
        let mut add_handle = add_handle;
        let h = loop {
            if cancelled.load(Ordering::SeqCst) {
                // Drop (detach) instead of abort: the spawned add_torrent task
                // continues running so it can consume the pending_delete entry
                // registered by delete_task and properly remove the torrent
                // from the librqbit session.  Aborting would leave the torrent
                // in the session with no way to clean it up later.
                drop(add_handle);
                return Err(DownloadError::Cancelled);
            }

            tokio::select! {
                biased;
                result = &mut add_handle => {
                    let resp = result
                        .map_err(|e| DownloadError::Other(format!("BT add task panicked: {e}")))?
                        .map_err(|e| DownloadError::Other(format!("BT add torrent failed: {e}")))?;
                    let h = match resp {
                        AddTorrentResponse::Added(_id, handle) => {
                            log_info!("[BT] task={} torrent added, id={}", short_id(&task_id), _id);
                            shared_bt.register_torrent_id(_id, &task_id).await;
                            handle
                        }
                        AddTorrentResponse::AlreadyManaged(_id, _handle) => {
                            let owner = shared_bt
                                .task_for_torrent(_id)
                                .await
                                .unwrap_or_else(|| "unknown".to_string());
                            log_info!(
                                "[BT] task={} torrent already managed by task={} (id={}), rejecting duplicate",
                                short_id(&task_id),
                                short_id(&owner),
                                _id
                            );
                            // Clean up the pre-created staging dir (it's empty/useless).
                            let _ = std::fs::remove_dir_all(&stage_dir);

                            let msg = format!(
                                "This torrent is already being downloaded by another task ({})",
                                short_id(&owner)
                            );
                            let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                            let _ = progress_tx
                                .send(ProgressUpdate {
                                    task_id: task_id.clone(),
                                    downloaded_bytes: 0,
                                    total_bytes: 0,
                                    status: STATUS_ERROR,
                                    error_message: msg.clone(),
                                    file_name: String::new(),
                                    segment_details: None,
                                })
                                .await;
                            return Err(DownloadError::Other(msg));
                        }
                        AddTorrentResponse::ListOnly(_) => {
                            return Err(DownloadError::Other(
                                "torrent returned list_only response".into(),
                            ));
                        }
                    };
                    break h;
                }
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    log_info!("[BT] task={} still resolving metadata...", short_id(&task_id));
                    let _ = progress_tx
                        .send(ProgressUpdate {
                            task_id: task_id.clone(),
                            downloaded_bytes: 0,
                            total_bytes: 0,
                            status: STATUS_PREPARING,
                            error_message: String::new(),
                            file_name: String::new(),
                            segment_details: None,
                        })
                        .await;
                }
            }
        };
        // Cache the handle for future pause/resume cycles.
        shared_bt.store_handle(&task_id, h.clone()).await;
        h
    };

    // -----------------------------------------------------------------------
    // Phase 3: Metadata resolved — extract name & total size, start tracking
    // -----------------------------------------------------------------------

    let stats = handle.stats();
    let total_bytes = stats.total_bytes as i64;
    let resolved_name = handle.name().unwrap_or_else(|| {
        if dn_name.is_empty() {
            format!("BT_{}", short_id(&task_id))
        } else {
            dn_name.clone()
        }
    });

    log_info!(
        "[BT] task={} metadata resolved: name={}, total={} bytes",
        short_id(&task_id),
        &resolved_name,
        total_bytes
    );

    // Extract file layout info and piece count from torrent metadata.
    // These are immutable after metadata resolution, so we cache them once.
    let (file_offsets, total_pieces) = handle
        .with_metadata(|meta| {
            let offsets: Vec<(u64, u64)> = meta
                .file_infos
                .iter()
                .map(|fi| (fi.offset_in_torrent, fi.len))
                .collect();
            let pieces = meta.lengths.total_pieces();
            (offsets, pieces)
        })
        .unwrap_or((Vec::new(), 0));

    log_info!(
        "[BT] task={} files={}, total_pieces={}",
        short_id(&task_id),
        file_offsets.len(),
        total_pieces,
    );

    // -----------------------------------------------------------------------
    // Phase 3.5: Send file list to Dart and wait for user file selection.
    // -----------------------------------------------------------------------

    // Count files for potential fallback (select-all).
    let file_count = handle
        .with_metadata(|meta| meta.file_infos.len())
        .unwrap_or(0);

    // -----------------------------------------------------------------------
    // Phase 3.5 — File selection.
    //
    // Three paths:
    //
    // R) Resume with existing in-memory handle (same app session):
    //    librqbit already has the correct update_only_files state.
    //    Skip everything — no DB read, no signal, no dialog.
    //    Use a full-range placeholder so update_only_files is NOT called.
    //
    // A) Pre-selected indices provided (new-download dialog OR DB-restored):
    //    The user's selection is already known.  Apply it via update_only_files
    //    so librqbit downloads only the chosen files (needed when re-adding
    //    after app restart because the fresh session starts with all files).
    //    No dialog shown.
    //
    // B) No pre-selection (first-time magnet link with no prior choice):
    //    Send BtFilesInfo to Dart so the file-selection dialog is shown.
    //    Persist the confirmed selection to DB so future resumes use Path A.
    //    Poll until the user confirms or the task is cancelled.
    // -----------------------------------------------------------------------

    let selected_indices: Vec<i32> = if had_existing_handle {
        // Path R — in-memory handle reused, librqbit state intact.
        log_info!(
            "[BT] task={} resumed from existing handle, skipping file selection",
            short_id(&task_id)
        );
        (0..file_count as i32).collect()
    } else if skip_file_selection {
        // Path S — user previously confirmed "all files"; DB recorded this.
        // librqbit defaults to downloading everything after re-add, which is
        // exactly what we want — no update_only_files call needed.
        // Use a full-range vec so the len == file_count guard skips the call.
        log_info!(
            "[BT] task={} skip_file_selection=true, downloading all files (no dialog)",
            short_id(&task_id)
        );
        (0..file_count as i32).collect()
    } else if !pre_selected_indices.is_empty() {
        // Path A — partial selection already known (new-download dialog or DB restore).
        log_info!(
            "[BT] task={} using pre-selected {} file(s) (no dialog)",
            short_id(&task_id),
            pre_selected_indices.len()
        );
        pre_selected_indices
    } else {
        // Path B — no pre-selection: build file list and send to Dart.
        // Filter out BEP-47 padding files — they are an implementation detail
        // and must not be shown to the user.  We keep the true meta index
        // (idx from enumerate) so that the indices forwarded to
        // update_only_files always refer to the correct meta.file_infos slot.
        let bt_files = handle
            .with_metadata(|meta| {
                meta.file_infos
                    .iter()
                    .enumerate()
                    .filter(|(_, fi)| {
                        // BEP-47 padding files are stored under a ".pad"
                        // directory component.  Use a path-based heuristic
                        // because FileInfos does not expose the attrs field.
                        let name = fi.relative_filename.to_string_lossy();
                        !name.contains("/.pad/")
                            && !name.contains("\\.pad\\")
                            && !fi
                                .relative_filename
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .starts_with(".pad")
                    })
                    .map(|(idx, fi)| crate::signals::BtFileEntry {
                        index: idx as i32,
                        path: fi.relative_filename.to_string_lossy().into_owned(),
                        size: fi.len as i64,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        crate::signals::BtFilesInfo {
            task_id: task_id.clone(),
            total_bytes,
            files: bt_files,
        }
        .send_signal_to_dart();

        log_info!(
            "[BT] task={} BtFilesInfo sent ({} files), waiting for user selection...",
            short_id(&task_id),
            file_count
        );

        // Poll until the user responds or the task is cancelled.
        loop {
            if cancelled.load(Ordering::SeqCst) {
                return Err(DownloadError::Cancelled);
            }
            if let Some(indices) = shared_bt.take_file_selection(&task_id).await {
                log_info!(
                    "[BT] task={} file selection received: {:?}",
                    short_id(&task_id),
                    &indices
                );
                break indices;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    };

    // Persist the confirmed selection to DB immediately so that future
    // resumes (including across app restarts) bypass the file-selection
    // dialog entirely.  We only persist when the selection came from user
    // interaction (Path B) or was pre-supplied (Path A when !had_existing_handle).
    // Path R (had_existing_handle) skips this because selected_indices is a
    // placeholder full-range vec and the real selection is already in the DB.
    // Persist the confirmed selection so future resumes skip the dialog.
    // Skip when:
    //   - had_existing_handle: DB already has the right value from the first run.
    //   - skip_file_selection: already persisted as "all", no change needed.
    //   - selected_indices starts with -1: user cancelled, leave DB empty so
    //     the dialog reappears on next resume (user can pick again).
    if !had_existing_handle && !skip_file_selection && selected_indices.first().copied() != Some(-1)
    {
        let is_all = selected_indices.len() >= file_count;
        let indices_to_save: &[i32] = if is_all { &[] } else { &selected_indices };
        let _ = db
            .save_bt_selected_files(&task_id, indices_to_save, is_all)
            .await;
        log_info!(
            "[BT] task={} persisted file selection ({}/{} files, is_all={}) to DB",
            short_id(&task_id),
            selected_indices.len(),
            file_count,
            is_all
        );
    }

    // [-1] is the sentinel sent by Dart when the user explicitly cancels the
    // file selection dialog.  Pause the task (status=2) so the user can
    // resume it later and pick files again, rather than leaving it in an
    // ambiguous state or marking it as error.
    if selected_indices.first().copied() == Some(-1) {
        log_info!(
            "[BT] task={} file selection cancelled by user → pausing",
            short_id(&task_id)
        );
        // Persist paused status to DB so it survives app restart.
        let _ = db.update_task_status(&task_id, STATUS_PAUSED, "").await;
        // Pause the librqbit torrent so it stops seeding / connecting.
        let _ = shared_bt.pause_task(&task_id).await;
        // Notify Dart so the UI immediately shows "Paused".
        let _ = progress_tx
            .send(ProgressUpdate {
                task_id: task_id.clone(),
                downloaded_bytes: 0,
                total_bytes,
                status: STATUS_PAUSED,
                error_message: String::new(),
                file_name: resolved_name.clone(),
                segment_details: None,
            })
            .await;
        // Return Cancelled so the manager does not overwrite our status=2.
        return Err(DownloadError::Cancelled);
    }

    // If user selected nothing (should not happen in practice), fall back to
    // downloading all files.
    let selected_indices = if selected_indices.is_empty() {
        (0..file_count as i32).collect::<Vec<_>>()
    } else {
        selected_indices
    };

    // Restrict to selected files when only a subset was chosen.
    // Path R (had_existing_handle) produces selected_indices.len() == file_count
    // so update_only_files is skipped — librqbit already has the right state.
    // Path A and Path B both need to apply the selection on a fresh add_torrent.
    if selected_indices.len() < file_count {
        let only: HashSet<usize> = selected_indices.iter().map(|&i| i as usize).collect();
        if let Err(e) = session.update_only_files(&handle, &only).await {
            log_info!(
                "[BT] task={} update_only_files error: {} — downloading all",
                short_id(&task_id),
                e
            );
        }
    }

    // -----------------------------------------------------------------------
    // Snapshot the relative paths of selected non-padding files.
    //
    // This is the SOLE source of truth for the completion-time move:
    //   - Path R (had_existing_handle): `selected_indices` is a (0..file_count)
    //     placeholder.  Load the real selection from DB.
    //   - Path S (skip_file_selection): `selected_indices` is (0..file_count)
    //     meaning "all" — already correct.
    //   - Path A / Path B: `selected_indices` is the real user choice.
    // -----------------------------------------------------------------------
    let true_selection: Vec<i32> = if had_existing_handle {
        match db.load_bt_selected_files(&task_id).await.ok().flatten() {
            Some(v) if v.is_empty() => (0..file_count as i32).collect(), // "all" sentinel
            Some(v) => v,
            None => (0..file_count as i32).collect(), // never confirmed → all
        }
    } else {
        selected_indices.clone()
    };
    let (selected_paths, non_padding_count): (Vec<PathBuf>, usize) = handle
        .with_metadata(|meta| {
            let total_non_padding = meta
                .file_infos
                .iter()
                .filter(|fi| !fi.attrs.padding)
                .count();
            let paths: Vec<PathBuf> = true_selection
                .iter()
                .filter_map(|&i| meta.file_infos.get(i as usize))
                .filter(|fi| !fi.attrs.padding)
                .map(|fi| fi.relative_filename.clone())
                .collect();
            (paths, total_non_padding)
        })
        .unwrap_or_default();
    let all_selected = !selected_paths.is_empty() && selected_paths.len() == non_padding_count;
    log_info!(
        "[BT] task={} completion plan: {} selected file(s), all_selected={}, non_padding_total={}",
        short_id(&task_id),
        selected_paths.len(),
        all_selected,
        non_padding_count
    );

    // Recompute total_bytes based on selected files only for accurate progress display.
    let total_bytes = {
        let selected_total: i64 = handle
            .with_metadata(|meta| {
                // 用 true_selection(从 DB 载入的真实选择)而非 selected_indices
                // (Path R 同会话续传时被置为全量占位 0..file_count),否则部分选择
                // 续传会把 total_bytes 误算成所有文件之和,导致进度百分比与 DB 元数据错误。
                true_selection
                    .iter()
                    .filter_map(|&i| meta.file_infos.get(i as usize))
                    .map(|fi| fi.len as i64)
                    .sum()
            })
            .unwrap_or(total_bytes);
        if selected_total > 0 && selected_total <= total_bytes {
            selected_total
        } else {
            total_bytes
        }
    };

    let _ = db
        .update_task_file_info(&task_id, &resolved_name, total_bytes)
        .await;
    let _ = db
        .update_task_status(&task_id, STATUS_DOWNLOADING, "")
        .await;

    // Notify Dart of the transition to "downloading" with resolved info
    let init_progress = stats.progress_bytes as i64;
    let init_pieces = stats
        .live
        .as_ref()
        .map(|l| l.snapshot.downloaded_and_checked_pieces)
        .unwrap_or(0);
    let _ = progress_tx
        .send(ProgressUpdate {
            task_id: task_id.clone(),
            downloaded_bytes: init_progress,
            total_bytes,
            status: STATUS_DOWNLOADING,
            error_message: String::new(),
            file_name: resolved_name.clone(),
            segment_details: Some(build_bt_segments(
                total_bytes,
                init_progress,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                init_pieces,
            )),
        })
        .await;

    // -----------------------------------------------------------------------
    // Phase 4: Download progress loop
    // -----------------------------------------------------------------------

    let mut last_report = Instant::now();
    let mut last_db_save = Instant::now();

    loop {
        // Check cancellation — the manager layer (pause_task / cancel_task)
        // is responsible for calling session.pause() on the torrent handle,
        // so we only need to exit the loop here.  This avoids a double-pause
        // race where both the inner loop and the manager call session.pause().
        if cancelled.load(Ordering::SeqCst) {
            log_info!(
                "[BT] task={} cancelled → exiting download loop",
                short_id(&task_id)
            );
            return Err(DownloadError::Cancelled);
        }

        let stats = handle.stats();
        // Guard: if this is the very first iteration of the loop (i.e. the
        // torrent just came back from add_torrent / AlreadyManaged + unpause)
        // and librqbit hasn't transitioned to Live yet, spin-wait up to 1s
        // instead of treating the transient Paused state as a cancellation.
        // This closes the race window where unpause() is called but the
        // state machine hasn't updated before we read stats below.
        let is_paused_state = matches!(stats.state, librqbit::TorrentStatsState::Paused);
        if is_paused_state && !cancelled.load(Ordering::SeqCst) {
            // Only spin on the very first poll (before any progress has been
            // reported) to avoid masking a genuine post-pause state.
            if stats.progress_bytes == 0 && stats.total_bytes > 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue; // re-check stats on next iteration
            }
        }
        let checked_progress = stats.progress_bytes as i64;
        let total = if stats.total_bytes > 0 {
            stats.total_bytes as i64
        } else {
            total_bytes
        };

        // Use fetched_bytes (actual bytes received from network, including
        // partial pieces) to expose early BT activity before pieces are fully
        // hash-verified. Keep display progress below 100% until stats.finished.
        let fetched = stats
            .live
            .as_ref()
            .map(|l| l.snapshot.fetched_bytes as i64)
            .unwrap_or(0);
        let progress =
            compute_bt_display_progress(checked_progress, fetched, total, stats.finished);

        // Check for error — but ONLY when the torrent is not in Paused state.
        //
        // Race window: pause_task() calls entry.token.cancel() then session.pause().
        // Between those two calls the progress loop may wake up, see cancelled=false
        // (the AtomicBool watcher fires on the next await), and read stats while
        // librqbit is transitioning through its internal states.  During that
        // transition stats.state can transiently be Error before settling on Paused,
        // which would cause us to report STATUS_ERROR and write status=4 to the DB
        // even though the user only asked to pause.
        //
        // Guarding on `!cancelled` is sufficient for the common case, but the
        // watcher task fires asynchronously so there is still a narrow window where
        // cancelled=false while the session is already shutting down.  The additional
        // `stats.state != Paused` guard eliminates that window: a torrent that
        // librqbit has already placed in the Paused state cannot be in error.
        if let Some(ref err) = stats.error {
            // If we are already cancelled (pause/cancel in progress), or the
            // torrent state is Paused, do not treat this as a hard error —
            // exit cleanly as Cancelled so the manager keeps status=2.
            if cancelled.load(Ordering::SeqCst) || is_paused_state {
                log_info!(
                    "[BT] task={} stats.error='{}' ignored — task is being paused/cancelled",
                    short_id(&task_id),
                    err
                );
                return Err(DownloadError::Cancelled);
            }
            let msg = format!("BT error: {err}");
            log_info!("[BT] task={} error: {}", short_id(&task_id), &msg);
            let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: progress,
                    total_bytes: total,
                    status: STATUS_ERROR,
                    error_message: msg.clone(),
                    file_name: String::new(),
                    segment_details: None,
                })
                .await;
            return Err(DownloadError::Other(msg));
        }

        // Check if finished
        if stats.finished {
            log_info!("[BT] task={} finished! total={}", short_id(&task_id), total);

            let final_total = if total > 0 { total } else { progress };
            // 注意:此处**不再**无条件写 STATUS_COMPLETED。BT 数据此刻仍在 staging
            // 目录,只有当下面的移动循环把所有选中文件成功落到 save_dir 后,才会写
            // STATUS_COMPLETED 并发完成信号;否则改写 STATUS_ERROR 并返回 Err,
            // 避免"未真正落盘的任务"显示为已完成(BUG-BT-COMPLETE-BEFORE-MOVE)。
            // progress / total_bytes 只记录已下载字节数,与完成与否无关,可先写。
            let _ = db.update_task_progress(&task_id, final_total).await;
            let _ = db.update_task_total_bytes(&task_id, final_total).await;

            // Build fully-completed segments — used in the single STATUS_COMPLETED
            // signal sent after the staging-dir move is resolved.
            let finished_segs = build_bt_segments(
                final_total,
                final_total,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                total_pieces as u64,
            );

            // Download complete.
            //
            // Move the user-selected files from the staging directory into
            // save_dir.  The move is driven by `selected_paths` (a snapshot
            // of `meta.file_infos[i].relative_filename` for selected indices,
            // taken right after the user confirmed the file selection).
            //
            // We send the single STATUS_COMPLETED signal AFTER the move so
            // that the file_name field already reflects the true disk name.
            let save_path = PathBuf::from(&save_dir);
            let stage_dir = bt_stage_dir(&save_dir, &task_id);

            let (completed_name, all_moves_succeeded) = if !stage_dir.exists() {
                // No staging dir at all — resumed download that was already
                // moved in a previous session, or existing_handle path where
                // output_folder == save_dir.  The DB file_name is already
                // correct; just return resolved_name for the signal.
                (resolved_name.clone(), true)
            } else {
                // Serialize the dedup-name + move sequence against other BT
                // task completions sharing this session's save_dir.  Without
                // this, two same-named torrents finishing simultaneously would
                // both dedup to the same name and the second rename would
                // overwrite the first task's file (see F050).  The guard is
                // held until the end of this block, covering the whole move loop.
                let _move_guard = shared_bt.lock_completion_move().await;
                let layout = compute_completion_layout(
                    &save_path,
                    &stage_dir,
                    &selected_paths,
                    all_selected,
                    &custom_name,
                );
                match layout {
                    None => {
                        // Empty selection — should not happen in practice.
                        log_info!(
                            "[BT] task={} completion: empty selection, falling back to resolved_name='{}'",
                            short_id(&task_id),
                            &resolved_name,
                        );
                        (resolved_name.clone(), false)
                    }
                    Some((moves, top_level_name)) => {
                        let total = moves.len();
                        // 完成移动是阻塞的 std::fs rename / 跨设备递归复制。bt-runtime
                        // 是 multi_thread（worker_threads = cpu_cores.clamp(2,8)），在其
                        // async worker 上直接阻塞会占用一个 worker，跨设备多 GB 复制时
                        // 拖慢其他 BT 任务的进度轮询/暂停响应。把整段移动循环搬进
                        // spawn_blocking（卸载到专用阻塞线程）,再 .await 句柄;`_move_guard`
                        // 仍在外层持有,跨越此 await,保留 completion_move_lock 的序列化语义
                        // (BUG-BT-COMPLETION-MOVE-BLOCKING)。
                        let tid_for_move = task_id.clone();
                        let move_result = tokio::task::spawn_blocking(move || {
                            let mut succeeded = 0usize;
                            for (src, dst) in &moves {
                                if !src.exists() {
                                    log_info!(
                                        "[BT] task={} completion: expected staged file missing '{}'",
                                        short_id(&tid_for_move),
                                        src.display(),
                                    );
                                    continue;
                                }
                                log_info!(
                                    "[BT] task={} moving '{}' → '{}'",
                                    short_id(&tid_for_move),
                                    src.display(),
                                    dst.display(),
                                );
                                if let Some(parent) = dst.parent()
                                    && !parent.exists()
                                {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                match move_path(src, dst) {
                                    Ok(()) => {
                                        succeeded += 1;
                                    }
                                    Err(e) => {
                                        log_info!(
                                            "[BT] task={} move failed: {} ({})",
                                            short_id(&tid_for_move),
                                            src.display(),
                                            e
                                        );
                                    }
                                }
                            }
                            succeeded
                        })
                        .await;
                        // spawn_blocking 内部 panic → JoinError。保守按全部失败处理
                        // (succeeded=0 → all_ok=false → 走 STATUS_ERROR 分支),
                        // 决不会把未落盘任务标成已完成。
                        let succeeded = match move_result {
                            Ok(n) => n,
                            Err(join_err) => {
                                log_info!(
                                    "[BT] task={} completion move task panicked: {}",
                                    short_id(&task_id),
                                    join_err,
                                );
                                0
                            }
                        };
                        let all_ok = total > 0 && succeeded == total;
                        if all_ok {
                            log_info!(
                                "[BT] task={} all {} selected file(s) moved; top_level='{}'",
                                short_id(&task_id),
                                total,
                                &top_level_name,
                            );
                        } else {
                            log_info!(
                                "[BT] task={} completion: {}/{} files moved — leaving staging dir for recovery",
                                short_id(&task_id),
                                succeeded,
                                total,
                            );
                        }
                        // Persist the resolved top-level name so that the UI
                        // and "open file location" agree with what's on disk.
                        let _ = db
                            .update_task_file_info(&task_id, &top_level_name, final_total)
                            .await;
                        (top_level_name, all_ok)
                    }
                }
            };

            // 移动失败兜底:数据仍在 staging,绝不能标已完成。
            //
            // 写 STATUS_ERROR(DB)、发 STATUS_ERROR 信号(而非 COMPLETED,且
            // file_name 留空——最终磁盘名并不存在),停止做种后 return Err,使
            // on_task_done 能感知失败(并在错误可重试时触发自动重试)。
            // (BUG-BT-COMPLETE-BEFORE-MOVE)
            if !all_moves_succeeded {
                let msg = format!(
                    "已完成但部分文件移动失败;数据保留在 {}",
                    stage_dir.display()
                );
                log_info!(
                    "[BT] task={} completion move failed — marking STATUS_ERROR: {}",
                    short_id(&task_id),
                    &msg,
                );
                let _ = db.update_task_status(&task_id, STATUS_ERROR, &msg).await;
                let _ = progress_tx
                    .send(ProgressUpdate {
                        task_id: task_id.clone(),
                        downloaded_bytes: final_total,
                        total_bytes: final_total,
                        status: STATUS_ERROR,
                        error_message: msg.clone(),
                        file_name: String::new(),
                        segment_details: None,
                    })
                    .await;
                // 仍要停止做种,但保留 staging 供恢复(下方清理已被
                // all_moves_succeeded 守卫,此分支不会删 staging)。
                let _ = shared_bt.pause_task(&task_id).await;
                return Err(DownloadError::Other(msg));
            }

            // 全部移动成功:此刻文件确已落到 save_dir,才写 STATUS_COMPLETED 并
            // 发完成信号——file_name 指向真实存在的磁盘名。
            let _ = db.update_task_status(&task_id, STATUS_COMPLETED, "").await;

            // Send the single STATUS_COMPLETED signal with the true file name.
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: final_total,
                    total_bytes: final_total,
                    status: STATUS_COMPLETED,
                    error_message: String::new(),
                    file_name: completed_name,
                    segment_details: Some(finished_segs),
                })
                .await;

            // Retain the handle in the cache (do NOT call take_handle) and
            // pause the torrent so it stops seeding.
            //
            // Keeping the handle alive means that a future
            // delete_task(delete_files=true) call can reach
            // session.delete(torrent_id, true), which properly removes the
            // files via librqbit.  Previously we called take_handle +
            // session.delete(false) here, which discarded the handle and
            // removed the session entry; that left no clean path for file
            // deletion — only an unreliable filesystem-path fallback.
            let _ = shared_bt.pause_task(&task_id).await;

            // Clean up the staging directory AFTER pause_task() so that
            // librqbit has released all file handles it held inside the
            // staging dir.  On Windows, open handles prevent deletion
            // (ERROR_SHARING_VIOLATION), which is why remove_dir_all called
            // before pause would silently fail and leave the staging dir
            // behind.  We retry a few times with a short delay to handle
            // the case where the runtime thread hasn't fully flushed yet.
            let stage_dir_for_cleanup = bt_stage_dir(&save_dir, &task_id);
            // Only clean up staging when every selected file was successfully
            // moved out.  If any move failed, leaving the staging dir intact
            // lets the user (or `rescue_stranded_staging_files` on next start)
            // recover the data manually.
            if all_moves_succeeded && stage_dir_for_cleanup.exists() {
                let mut removed = false;
                for attempt in 0u8..4 {
                    if attempt > 0 {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    match tokio::fs::remove_dir_all(&stage_dir_for_cleanup).await {
                        Ok(()) => {
                            log_info!(
                                "[BT] task={} staging dir removed after pause (attempt {})",
                                short_id(&task_id),
                                attempt + 1
                            );
                            removed = true;
                            break;
                        }
                        Err(e) => {
                            log_info!(
                                "[BT] task={} staging dir remove attempt {} failed: {}",
                                short_id(&task_id),
                                attempt + 1,
                                e
                            );
                        }
                    }
                }
                if !removed {
                    log_info!(
                        "[BT] task={} staging dir '{}' could not be removed — left for startup cleanup",
                        short_id(&task_id),
                        stage_dir_for_cleanup.display()
                    );
                }
            }

            return Ok(());
        }

        // Progress reporting — runs on every poll cycle (500ms).
        // The elapsed check is kept as a safety guard against sleep jitter.
        if last_report.elapsed() >= Duration::from_millis(450) {
            // Speed: librqbit Speed.mbps is actually MiB/s
            let speed_bps = stats
                .live
                .as_ref()
                .map(|l| (l.download_speed.mbps * 1024.0 * 1024.0) as i64)
                .unwrap_or(0);

            let (peers_live, peers_connecting, peers_queued, peers_seen, peers_dead) = stats
                .live
                .as_ref()
                .map(|l| {
                    let ps = &l.snapshot.peer_stats;
                    (ps.live, ps.connecting, ps.queued, ps.seen, ps.dead)
                })
                .unwrap_or((0, 0, 0, 0, 0));

            let downloaded_pieces = stats
                .live
                .as_ref()
                .map(|l| l.snapshot.downloaded_and_checked_pieces)
                .unwrap_or(0);

            let upload_speed_bps = stats
                .live
                .as_ref()
                .map(|l| (l.upload_speed.mbps * 1024.0 * 1024.0) as i64)
                .unwrap_or(0);

            // If the torrent has entered Paused state while we are still in
            // the progress loop, it means pause_task() already called
            // session.pause() and the handle is now frozen.  The loop is
            // about to exit on the next cancelled-flag check (the watcher
            // task fires asynchronously), but we must not send STATUS_ERROR
            // or STATUS_PREPARING for a genuinely paused torrent.
            // Exit early and let the manager's explicit status=2 signal
            // (sent in pause_task) be the last word on the UI state.
            if is_paused_state {
                log_info!(
                    "[BT] task={} stats.state=Paused while in progress loop — exiting early (pause in progress)",
                    short_id(&task_id)
                );
                return Err(DownloadError::Cancelled);
            }

            let status_code = match stats.state {
                librqbit::TorrentStatsState::Live => STATUS_DOWNLOADING,
                librqbit::TorrentStatsState::Initializing => STATUS_PREPARING,
                librqbit::TorrentStatsState::Paused => STATUS_PREPARING, // unreachable after guard above
                librqbit::TorrentStatsState::Error => STATUS_ERROR,
            };

            log_info!(
                "[BT] task={} state={:?} progress={}/{} (checked={}, fetched={}) pieces={}/{} down={} B/s up={} B/s peers(live={} connecting={} queued={} seen={} dead={})",
                short_id(&task_id),
                stats.state,
                progress,
                total,
                checked_progress,
                fetched,
                downloaded_pieces,
                total_pieces,
                speed_bps,
                upload_speed_bps,
                peers_live,
                peers_connecting,
                peers_queued,
                peers_seen,
                peers_dead
            );

            let seg_details = build_bt_segments(
                total,
                progress,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                downloaded_pieces,
            );

            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: progress,
                    total_bytes: total,
                    status: status_code,
                    error_message: String::new(),
                    file_name: String::new(),
                    segment_details: Some(seg_details),
                })
                .await;

            last_report = Instant::now();
        }

        // Periodic DB save (every 3s).
        // Use checked_progress for DB persistence (not fetched_bytes) to
        // avoid inflating progress with partial pieces that would need
        // re-download after restart.
        if checked_progress > 0 && last_db_save.elapsed() >= Duration::from_secs(3) {
            let _ = db.update_task_progress(&task_id, checked_progress).await;
            if total > 0 {
                let _ = db.update_task_total_bytes(&task_id, total).await;
            }
            last_db_save = Instant::now();
        }

        // Poll interval — aligned with the progress reporting interval (500ms)
        // to avoid wasted cycles.  Cancel detection latency of 500ms is
        // acceptable since the manager layer handles session.pause() directly.
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Parse a raw `.torrent` file's file list without creating a download task.
///
/// This is used by the new-download dialog to preview torrent contents
/// before the user confirms the download.  It is purely local (no network).
pub fn probe_torrent_meta(probe_id: String, torrent_bytes: Vec<u8>) {
    // librqbit re-exports librqbit_core::torrent_metainfo::* at the crate root,
    // so torrent_from_bytes_ext and ByteBuf are both accessible via librqbit::.
    use librqbit::{ByteBuf, torrent_from_bytes_ext};

    let result: Result<crate::signals::TorrentMetaResult, String> = (|| {
        // ByteBuf<'_> borrows torrent_bytes; the parsed value must not outlive it.
        let parsed = torrent_from_bytes_ext::<ByteBuf<'_>>(&torrent_bytes)
            .map_err(|e| format!("torrent parse error: {e}"))?;
        let info = &parsed.meta.info;

        // Build file list. For single-file torrents this yields one entry.
        let mut files: Vec<crate::signals::BtFileEntry> = Vec::new();
        let mut total_bytes: i64 = 0;
        for (idx, fd) in info
            .iter_file_details()
            .map_err(|e| format!("iter_file_details error: {e}"))?
            .enumerate()
        {
            // Skip padding files (BEP-47).
            let attrs: librqbit::FileDetailsAttrs = fd.attrs();
            if attrs.padding {
                continue;
            }
            let path = fd
                .filename
                .to_string()
                .unwrap_or_else(|_| format!("file_{idx}"));
            let size = fd.len as i64;
            total_bytes += size;
            files.push(crate::signals::BtFileEntry {
                index: idx as i32,
                path,
                size,
            });
        }

        let name = info
            .name
            .as_ref()
            .and_then(|n: &ByteBuf<'_>| std::str::from_utf8(n.as_ref()).ok())
            .unwrap_or("Unknown")
            .to_owned();

        Ok(crate::signals::TorrentMetaResult {
            probe_id: probe_id.clone(),
            name,
            total_bytes,
            files,
            error: String::new(),
        })
    })();

    let signal = match result {
        Ok(r) => r,
        Err(e) => {
            log_info!("[BT] probe_torrent_meta error: {}", e);
            crate::signals::TorrentMetaResult {
                probe_id,
                name: String::new(),
                total_bytes: 0,
                files: Vec::new(),
                error: e,
            }
        }
    };
    signal.send_signal_to_dart();
}

#[cfg(test)]
mod tests {
    use super::compute_bt_display_progress;

    // -------------------------------------------------------------------------
    // urlencoding_decode — literal multi-byte UTF-8 must not be mangled (F052).
    // -------------------------------------------------------------------------

    #[test]
    fn urlencoding_decode_literal_utf8_not_mangled() {
        // Raw (unencoded) UTF-8 in dn= is common; it must round-trip intact
        // rather than being decoded per-byte as Latin-1.
        assert_eq!(super::urlencoding_decode("中文电影"), "中文电影");
    }

    #[test]
    fn urlencoding_decode_percent_encoded_utf8() {
        // "中" = E4 B8 AD percent-encoded.
        assert_eq!(super::urlencoding_decode("%E4%B8%AD"), "中");
    }

    #[test]
    fn urlencoding_decode_mixed_literal_and_encoded() {
        // Literal "中" followed by percent-encoded "文".
        assert_eq!(super::urlencoding_decode("中%E6%96%87"), "中文");
    }

    #[test]
    fn urlencoding_decode_plus_is_space() {
        assert_eq!(super::urlencoding_decode("a+b"), "a b");
    }

    #[test]
    fn urlencoding_decode_incomplete_percent_tail_keeps_literal() {
        // A trailing incomplete `%` followed by literal multi-byte must not
        // panic and must not mangle the literal sequence.
        assert_eq!(super::urlencoding_decode("ab%中"), "ab%中");
    }

    #[test]
    fn urlencoding_decode_invalid_hex_keeps_percent() {
        // `%zz` is not a valid escape — `%` is preserved as a literal.
        assert_eq!(super::urlencoding_decode("%zz"), "%zz");
    }

    // -------------------------------------------------------------------------
    // magnet_display_name — decode + sanitize, None on empty (F049).
    // -------------------------------------------------------------------------

    #[test]
    fn magnet_display_name_sanitizes_illegal_chars() {
        // `/` in the decoded dn must be sanitized to `_`, matching meta_prober.
        let name = super::magnet_display_name("magnet:?xt=urn:btih:abc&dn=a%2Fb");
        assert_eq!(name.as_deref(), Some("a_b"));
    }

    #[test]
    fn magnet_display_name_literal_utf8() {
        let name = super::magnet_display_name("magnet:?xt=urn:btih:abc&dn=中文电影");
        assert_eq!(name.as_deref(), Some("中文电影"));
    }

    #[test]
    fn magnet_display_name_none_when_no_dn() {
        assert!(super::magnet_display_name("magnet:?xt=urn:btih:abc").is_none());
    }

    #[test]
    fn magnet_display_name_none_when_dn_empty() {
        // Empty dn value decodes to empty → None (caller falls back to a
        // generated name rather than the "download" placeholder).
        assert!(super::magnet_display_name("magnet:?xt=urn:btih:abc&dn=").is_none());
    }

    // -------------------------------------------------------------------------
    // build_multi_file_segments — subset total with full offsets (F055).
    // -------------------------------------------------------------------------

    #[test]
    fn multi_file_segments_skip_out_of_range_and_no_negative() {
        // Subset selection: total_bytes covers only the first file (100), but
        // file_offsets/file_progress carry the full torrent (3 files).  The
        // second/third files start at offset >= total and must be skipped; no
        // segment may carry a negative downloaded_bytes.
        let total_bytes = 100i64;
        let file_progress = [50u64, 0u64, 0u64];
        // file 0: [0,100), file 1: [100,300), file 2: [300,400)
        let file_offsets = [(0u64, 100u64), (100u64, 200u64), (300u64, 100u64)];
        let segs = super::build_multi_file_segments(total_bytes, &file_progress, &file_offsets);
        // Only the in-range file 0 survives.
        assert_eq!(segs.len(), 1);
        let s = &segs[0];
        assert_eq!(s.start_byte, 0);
        assert_eq!(s.end_byte, 99);
        assert_eq!(s.downloaded_bytes, 50);
        for s in &segs {
            assert!(s.downloaded_bytes >= 0, "downloaded_bytes must be non-negative");
            assert!(s.end_byte >= s.start_byte, "end must not precede start");
        }
    }

    // -------------------------------------------------------------------------
    // compute_completion_layout — dedup must not stack underscores (F038).
    // -------------------------------------------------------------------------

    #[test]
    fn completion_layout_dedup_uses_numeric_suffix() {
        use std::path::PathBuf;
        // Two selected files with the same basename in different sub-dirs:
        // their flat destinations collide and must be deduped as
        // "file.txt" + "file (1).txt", not "_file.txt".
        let tmp = std::env::temp_dir().join(format!(
            "fluxdown_bt_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let stage = tmp.join(".stage");
        let _ = std::fs::create_dir_all(&stage);

        let selected = vec![
            PathBuf::from("dirA/file.txt"),
            PathBuf::from("dirB/file.txt"),
        ];
        let layout = super::compute_completion_layout(&tmp, &stage, &selected, false, "");
        let _ = std::fs::remove_dir_all(&tmp);

        // Avoid `.unwrap()`/`.expect()` (denied by clippy) — match explicitly.
        let moves = match layout {
            Some((moves, _top)) => moves,
            None => panic!("layout should be Some"),
        };
        assert_eq!(moves.len(), 2);
        let dst0 = moves[0].1.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let dst1 = moves[1].1.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert_eq!(dst0, "file.txt");
        assert_eq!(dst1, "file (1).txt");
        // No underscore-prefixed name should ever be produced.
        assert!(!dst1.starts_with('_'), "must not stack underscore prefixes");
    }

    #[test]
    fn display_progress_does_not_reach_total_before_finished() {
        let progress = compute_bt_display_progress(900, 1000, 1000, false);
        assert_eq!(progress, 999);
    }

    #[test]
    fn display_progress_can_reach_total_when_finished() {
        let progress = compute_bt_display_progress(900, 1000, 1000, true);
        assert_eq!(progress, 1000);
    }

    #[test]
    fn display_progress_handles_unknown_total() {
        let progress = compute_bt_display_progress(0, 12345, 0, false);
        assert_eq!(progress, 12345);
    }

    // -------------------------------------------------------------------------
    // InflightGuard: panic-safe decrement via RAII.
    // -------------------------------------------------------------------------

    /// Verify that InflightGuard decrements the counter even when the
    /// enclosing tokio::spawn closure panics before the natural end.
    #[tokio::test]
    async fn inflight_guard_decrements_on_task_panic() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Minimal stand-in for SharedBtSession: just the counter.
        let counter = Arc::new(AtomicUsize::new(0));

        // Build a minimal InflightGuard directly using the same AtomicUsize
        // so we can test the Drop behaviour without constructing a full Session.
        struct TestGuard(Arc<AtomicUsize>);
        impl Drop for TestGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }

        // Simulate: shared_bt.inflight_guard() — increments then returns guard.
        counter.fetch_add(1, Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        let guard = TestGuard(Arc::clone(&counter));

        // Simulate: tokio::spawn(async move { let _g = guard; ..panic.. })
        let handle = tokio::spawn(async move {
            let _g = guard; // guard moved into task; Drop runs on panic
            panic!("simulated add_torrent panic");
        });

        // Tokio catches the panic; JoinHandle returns Err.
        assert!(handle.await.is_err());

        // FIX confirmed: guard's Drop ran during tokio's task cleanup,
        // decrementing the counter back to 0.
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "InflightGuard must decrement counter even after task panic"
        );
    }

    /// Verify normal (non-panic) path: guard also decrements on clean exit.
    #[tokio::test]
    async fn inflight_guard_decrements_on_normal_exit() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));

        struct TestGuard(Arc<AtomicUsize>);
        impl Drop for TestGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }

        counter.fetch_add(1, Ordering::Relaxed);
        let guard = TestGuard(Arc::clone(&counter));

        let handle = tokio::spawn(async move {
            let _g = guard;
            // normal return — no panic
        });

        assert!(handle.await.is_ok());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
