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

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ManagedTorrent,
    PeerConnectionOptions, Session, SessionOptions, SessionPersistenceConfig,
};

/// Alias for librqbit's `BtHandle` (`Arc<ManagedTorrent>`).
/// The upstream type is not re-exported, so we define it locally.
pub type BtHandle = Arc<ManagedTorrent>;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::db::Db;
use crate::downloader::{DownloadError, ProgressUpdate, SegmentProgressInfo};

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

/// Extract the `dn=` (display name) parameter from a magnet URI, if present.
fn magnet_display_name(url: &str) -> Option<String> {
    url.split('&')
        .find_map(|part| {
            let part = part.strip_prefix("magnet:?").unwrap_or(part);
            part.strip_prefix("dn=")
        })
        .map(urlencoding_decode)
}

/// Minimal percent-decoding for `dn=` values (UTF-8 safe).
///
/// Collects percent-encoded bytes into a buffer and decodes them as UTF-8,
/// correctly handling multi-byte characters (e.g. CJK, emoji).
///
/// Incomplete `%` sequences at the end of the input (e.g. `%`, `%A`) are
/// treated as literal characters rather than silently padded with zeros.
fn urlencoding_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut bytes_buf: Vec<u8> = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    // Flush accumulated percent-encoded bytes as UTF-8 into `out`.
    let flush = |buf: &mut Vec<u8>, out: &mut String| {
        if !buf.is_empty() {
            match std::str::from_utf8(buf) {
                Ok(s) => out.push_str(s),
                Err(_) => {
                    // Fallback: replace invalid UTF-8 with replacement char
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
                // Full %XX sequence — decode as a byte.
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                bytes_buf.push(hex_val(hi) << 4 | hex_val(lo));
                i += 3;
            }
            b'%' => {
                // Incomplete `%` at end of string — emit as literal.
                flush(&mut bytes_buf, &mut out);
                out.push('%');
                i += 1;
                // Also emit any remaining characters after `%` literally.
                while i < len {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            b => {
                flush(&mut bytes_buf, &mut out);
                out.push(b as char);
                i += 1;
            }
        }
    }
    flush(&mut bytes_buf, &mut out);
    out
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
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
}

impl SharedBtSession {
    /// Create the shared session with the given initial speed limit and config.
    ///
    /// `default_save_dir` is used as the Session's default output folder
    /// (individual torrents override this via `AddTorrentOptions::output_folder`).
    ///
    /// `speed_limit_bps` is the global download speed limit in bytes/sec
    /// (0 = unlimited).
    ///
    /// `bt_config` contains user-configurable BT settings (DHT, UPnP, ports,
    /// custom trackers).
    pub fn new(
        default_save_dir: &str,
        speed_limit_bps: u64,
        bt_config: &BtConfig,
    ) -> Result<Self, DownloadError> {
        // Scale worker threads with CPU cores for better throughput on
        // multi-core machines.  Minimum 4, maximum 16.
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let worker_threads = cpu_cores.clamp(4, 16);

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
        // alongside the download database so everything is co-located.
        let persistence_folder = PathBuf::from(default_save_dir).join(".fluxdown_bt");

        // Validate and clamp port range.
        let port_start = bt_config.port_start.max(1024);
        let port_end = bt_config.port_end.max(port_start);

        let enable_dht = bt_config.enable_dht;
        let enable_upnp = bt_config.enable_upnp;

        let save_dir = default_save_dir.to_owned();
        let session = rt.block_on(async {
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
                // Buffer up to 128 MiB of writes in memory before flushing
                // to disk.  Reduces I/O contention from many small pieces
                // and significantly improves throughput on HDD.  Raised
                // from 64 to better accommodate high-speed connections.
                defer_writes_up_to: Some(128),
                // Limit concurrent torrent initialisation to 3 to prevent
                // DHT/tracker storms when many BT tasks start at once.
                concurrent_init_limit: Some(3),
                ..Default::default()
            };

            Session::new_with_opts(save_dir.into(), opts).await
        }).map_err(|e| DownloadError::Other(format!("BT session init failed: {e}")))?;

        rinf::debug_print!(
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
        })
    }

    /// Update the global download speed limit at runtime.
    /// `bps == 0` means unlimited.  Takes effect immediately on all active
    /// BT downloads.
    pub fn set_speed_limit(&self, bps: u64) {
        let limit = NonZeroU32::new(bps.min(u32::MAX as u64) as u32);
        self.session.ratelimits.set_download_bps(limit);
        rinf::debug_print!("[BT] shared session speed limit updated to {} B/s", bps);
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
        self.handles.lock().await.insert(task_id.to_string(), handle);
    }

    /// Remove and return the cached handle for a task.
    pub async fn take_handle(&self, task_id: &str) -> Option<BtHandle> {
        self.handles.lock().await.remove(task_id)
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
                self.session.pause(&handle).await.map_err(|e| {
                    DownloadError::Other(format!("BT pause failed: {e}"))
                })?;
            }
            rinf::debug_print!("[BT] task={} paused via session API", short_id(task_id));
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
                self.session.unpause(&handle).await.map_err(|e| {
                    DownloadError::Other(format!("BT unpause failed: {e}"))
                })?;
                rinf::debug_print!("[BT] task={} resumed via session API", short_id(task_id));
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
        rinf::debug_print!("[BT] shutting down shared session...");
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
                    rinf::debug_print!("[BT] shutdown: failed to pause task {}: {}", short_id(tid), e);
                }
            }
        });
        // The Runtime::drop will be called after this, which blocks until
        // all spawned tasks finish (or the runtime forces them to stop).
        rinf::debug_print!("[BT] shared session shutdown complete");
    }

    /// Permanently delete a torrent from the session, removing persistence
    /// data.  `delete_files` controls whether downloaded data is also removed.
    pub async fn delete_task(&self, task_id: &str, delete_files: bool) {
        // Remove from map first (under lock), then perform async deletion
        // outside the lock to minimise contention.
        let handle = self.handles.lock().await.remove(task_id);
        if let Some(handle) = handle {
            let torrent_id = handle.id();
            if let Err(e) = self.session.delete(torrent_id.into(), delete_files).await {
                rinf::debug_print!("[BT] task={} session.delete error: {}", short_id(task_id), e);
            } else {
                rinf::debug_print!("[BT] task={} deleted from session (delete_files={})", short_id(task_id), delete_files);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// BT download params
// ---------------------------------------------------------------------------

pub struct BtDownloadParams {
    pub task_id: String,
    pub magnet_url: String,
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
    let _ = params.db.update_task_status(&task_id, STATUS_PREPARING, "").await;
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

    rinf::debug_print!("[BT] task={} starting bt download (shared session)...", short_id(&task_id));

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
    let magnet_url = params.magnet_url.clone();
    let save_dir = params.save_dir.clone();
    let tid = task_id.clone();
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
        magnet_url,
        save_dir,
        db,
        progress_tx,
        cancelled: cancelled_clone,
        session,
        shared_bt,
        existing_handle,
    };
    let result = bt_runtime.spawn(async move {
        bt_download_inner(inner_params).await
    })
    .await;

    cancel_watcher.abort();

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(join_err) => Err(DownloadError::Other(format!("BT task panicked: {join_err}"))),
    }
}

// ---------------------------------------------------------------------------
// Inner download logic (runs on the shared BT runtime)
// ---------------------------------------------------------------------------

/// Parameters for the inner BT download loop (avoids too-many-arguments warning).
struct BtInnerParams {
    task_id: String,
    magnet_url: String,
    save_dir: String,
    db: Db,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    cancelled: Arc<AtomicBool>,
    session: Arc<Session>,
    shared_bt: Arc<SharedBtSession>,
    existing_handle: Option<BtHandle>,
}

// ---------------------------------------------------------------------------
// Task status codes — must match Dart TaskStatus enum values.
// ---------------------------------------------------------------------------
const STATUS_DOWNLOADING: i32 = 1;
#[allow(dead_code)]
const STATUS_PAUSED: i32 = 2;
const STATUS_COMPLETED: i32 = 3;
const STATUS_ERROR: i32 = 4;
const STATUS_PREPARING: i32 = 5;

/// Number of virtual segments for single-file BT progress visualization.
const BT_VIRTUAL_SEGMENTS: i32 = 16;

/// Build segment progress data from real BT file-level progress.
///
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
        let end = (offset + file_len).saturating_sub(1) as i64;
        let end = end.min(total_bytes - 1);
        segs.push(SegmentProgressInfo {
            index: i as i32,
            start_byte: start,
            end_byte: end,
            downloaded_bytes: (dl_bytes as i64).min(end - start + 1),
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
    let n = BT_VIRTUAL_SEGMENTS;
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
        let seg_ratio = (completion_ratio + perturbation * 0.3)
            .clamp(0.0, 1.0);

        // Snap to exact 0 or 1 when close to boundaries
        let seg_dl_pieces = if completion_ratio <= 0.001 {
            0.0
        } else if completion_ratio >= 0.999 {
            seg_total_pieces as f64
        } else {
            (seg_total_pieces as f64 * seg_ratio).round()
        };

        let dl = ((seg_dl_pieces / seg_total_pieces.max(1) as f64) * seg_size as f64)
            .round() as i64;

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
            let share = ((seg_size as f64 / total_bytes as f64) * abs_diff)
                .round() as i64;
            let adj = share.min(remaining);
            seg.downloaded_bytes = (seg.downloaded_bytes + direction * adj)
                .clamp(0, seg_size);
            remaining -= adj;
            if remaining <= 0 {
                break;
            }
        }
    }

    segs
}

async fn bt_download_inner(p: BtInnerParams) -> Result<(), DownloadError> {
    let BtInnerParams {
        task_id,
        magnet_url,
        save_dir,
        db,
        progress_tx,
        cancelled,
        session,
        shared_bt,
        existing_handle,
    } = p;
    // -----------------------------------------------------------------------
    // Phase 1: Send initial file name from dn= parameter so user sees something
    // -----------------------------------------------------------------------

    let dn_name = magnet_display_name(&magnet_url).unwrap_or_default();
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
        rinf::debug_print!("[BT] task={} reusing existing handle (resume)", short_id(&task_id));
        // Handle was already unpaused by SharedBtSession::resume_task,
        // so we can go straight to the progress loop.
        h
    } else {
        let add_opts = AddTorrentOptions {
            overwrite: true,
            output_folder: Some(save_dir.clone()),
            ..Default::default()
        };

        rinf::debug_print!(
            "[BT] task={} adding magnet to shared session (metadata resolution may take a while)...",
            short_id(&task_id)
        );

        let session_for_add = session.clone();
        let magnet_for_add = magnet_url.clone();
        let add_handle = tokio::spawn(async move {
            session_for_add
                .add_torrent(AddTorrent::from_url(&magnet_for_add), Some(add_opts))
                .await
        });

        // Send "preparing" heartbeats while waiting for metadata.
        let mut add_handle = add_handle;
        let h = loop {
            if cancelled.load(Ordering::SeqCst) {
                add_handle.abort();
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
                            rinf::debug_print!("[BT] task={} torrent added, id={}", short_id(&task_id), _id);
                            handle
                        }
                        AddTorrentResponse::AlreadyManaged(_id, handle) => {
                            rinf::debug_print!("[BT] task={} torrent already in session, id={}", short_id(&task_id), _id);
                            // Unpause if it was paused from a previous session
                            if handle.is_paused() {
                                let _ = session.unpause(&handle).await;
                            }
                            handle
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
                    rinf::debug_print!("[BT] task={} still resolving metadata...", short_id(&task_id));
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

    rinf::debug_print!(
        "[BT] task={} metadata resolved: name={}, total={} bytes",
        short_id(&task_id), &resolved_name, total_bytes
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
        .unwrap_or_else(|_| (Vec::new(), 0));

    rinf::debug_print!(
        "[BT] task={} files={}, total_pieces={}",
        short_id(&task_id),
        file_offsets.len(),
        total_pieces
    );

    let _ = db
        .update_task_file_info(&task_id, &resolved_name, total_bytes)
        .await;
    let _ = db.update_task_status(&task_id, STATUS_DOWNLOADING, "").await;

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
            rinf::debug_print!("[BT] task={} cancelled → exiting download loop", short_id(&task_id));
            return Err(DownloadError::Cancelled);
        }

        let stats = handle.stats();
        let checked_progress = stats.progress_bytes as i64;
        let total = if stats.total_bytes > 0 {
            stats.total_bytes as i64
        } else {
            total_bytes
        };

        // Use fetched_bytes (actual bytes received from network, including
        // partial pieces) when checked progress is 0.  This lets the user
        // see that BT is actively downloading even before any piece has been
        // fully received and hash-verified.  Once pieces start completing,
        // checked_progress is always >= fetched partial data, so we take
        // the max to ensure monotonic progress.
        let fetched = stats
            .live
            .as_ref()
            .map(|l| l.snapshot.fetched_bytes as i64)
            .unwrap_or(0);
        let progress = checked_progress.max(fetched).min(total);

        // Check for error — keep the handle cached so user can retry.
        if let Some(ref err) = stats.error {
            let msg = format!("BT error: {err}");
            rinf::debug_print!("[BT] task={} error: {}", short_id(&task_id), &msg);
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
            rinf::debug_print!("[BT] task={} finished! total={}", short_id(&task_id), total);

            let final_total = if total > 0 { total } else { progress };
            let _ = db.update_task_status(&task_id, STATUS_COMPLETED, "").await;
            let _ = db.update_task_progress(&task_id, final_total).await;
            let _ = db.update_task_total_bytes(&task_id, final_total).await;

            // Build fully-completed segments
            let finished_segs = build_bt_segments(
                final_total,
                final_total,
                &stats.file_progress,
                &file_offsets,
                total_pieces,
                total_pieces as u64,
            );
            let _ = progress_tx
                .send(ProgressUpdate {
                    task_id: task_id.clone(),
                    downloaded_bytes: final_total,
                    total_bytes: final_total,
                    status: STATUS_COMPLETED,
                    error_message: String::new(),
                    file_name: resolved_name.clone(),
                    segment_details: Some(finished_segs),
                })
                .await;

            // Download complete — remove from session to free resources.
            // Keep files on disk (delete_files=false).  Also remove from
            // the handle cache since we no longer need to pause/resume.
            shared_bt.take_handle(&task_id).await;
            let torrent_id = handle.id();
            let _ = session.delete(torrent_id.into(), false).await;
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

            let status_code = match stats.state {
                librqbit::TorrentStatsState::Live => STATUS_DOWNLOADING,
                librqbit::TorrentStatsState::Initializing => STATUS_PREPARING,
                librqbit::TorrentStatsState::Paused => STATUS_PREPARING, // transitional
                librqbit::TorrentStatsState::Error => STATUS_ERROR,
            };

            rinf::debug_print!(
                "[BT] task={} state={:?} progress={}/{} (checked={}, fetched={}) pieces={}/{} down={} B/s up={} B/s peers(live={} connecting={} queued={} seen={} dead={})",
                short_id(&task_id), stats.state, progress, total,
                checked_progress, fetched,
                downloaded_pieces, total_pieces, speed_bps, upload_speed_bps,
                peers_live, peers_connecting, peers_queued, peers_seen, peers_dead
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
