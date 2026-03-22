//! Dynamic segment (thread) allocation for downloads.
//!
//! Instead of a hard-coded "8 segments", this module calculates an optimal
//! segment count based on:
//!
//! 1. **File size** — tiny files need fewer segments; each segment should get
//!    at least [`MIN_BYTES_PER_SEGMENT`] bytes to amortize HTTP overhead.
//! 2. **CPU logical cores** — more segments than cores is wasteful for I/O
//!    scheduling.
//! 3. **Observed bandwidth** — measured during the probe phase.  High-latency
//!    / low-bandwidth links benefit from fewer connections.

use std::time::Duration;

use reqwest::Client;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Absolute lower bound — even "auto" never goes below this.
const MIN_SEGMENTS: i32 = 1;

/// Absolute upper bound — caps runaway calculations.
const MAX_SEGMENTS: i32 = 64;

/// Each segment should handle at least 1 MB to amortize HTTP/TLS handshake
/// and TCP slow-start overhead.  Modern TLS 1.3 completes in ~1 RTT, so the
/// handshake cost for 1 MB segments is negligible (<1% on most links).
/// Lowered from 2 MB to allow more segments on 10–20 MB files, matching
/// IDM's aggressive segmentation behaviour.
const MIN_BYTES_PER_SEGMENT: i64 = 1024 * 1024; // 1 MB

/// Files ≤ this size are always single-segment (no benefit from splitting).
/// Lowered from 4 MB to 2 MB: on broadband connections even 2–4 MB files
/// benefit from 2 parallel segments (halves the wall-clock time).
const SINGLE_SEGMENT_THRESHOLD: i64 = 2 * 1024 * 1024; // 2 MB

/// How many bytes to download during the bandwidth probe.
/// 128 KB 已足以退出 TCP 慢启动（典型 RTT 30-100ms，慢启动 ~5 RTTs 约 50KB），
/// 同时将探测耗时从 ~1.7s（512KB/300KB/s）降低至 ~400ms，显著改善批量任务启动体验。
const PROBE_BYTES: u64 = 128 * 1024; // 128 KB

/// Maximum wall-clock time for the bandwidth probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// Bandwidth thresholds (bytes/sec) for segment scaling.
/// - Below LOW  → few segments (connection is the bottleneck, not parallelism)
/// - Above HIGH → many segments (we can saturate each connection)
const BW_LOW: f64 = 512.0 * 1024.0; //  512 KB/s
const BW_MED: f64 = 5.0 * 1024.0 * 1024.0; //    5 MB/s
const BW_HIGH: f64 = 50.0 * 1024.0 * 1024.0; //   50 MB/s

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Inputs collected before the download starts.
pub struct AdvisorInput {
    /// Total file size in bytes (from `Content-Length`).  0 or negative means
    /// unknown — we fall back to a safe default.
    pub total_bytes: i64,
    /// Whether the server supports HTTP Range requests.
    pub supports_range: bool,
}

/// The advisor's recommendation.
#[derive(Debug, Clone)]
pub struct SegmentAdvice {
    /// Recommended number of segments.
    pub segments: i32,
    /// Human-readable explanation (for debug logging).
    pub reason: String,
}

/// Calculate the optimal segment count **without** a bandwidth probe.
///
/// This is the fast path: uses only file size + CPU cores.
/// Called when bandwidth info is not yet available.
pub fn advise_static(input: &AdvisorInput) -> SegmentAdvice {
    // Unknown size or no range support → single segment.
    if input.total_bytes <= 0 || !input.supports_range {
        return SegmentAdvice {
            segments: 1,
            reason: if !input.supports_range {
                "server does not support Range requests".into()
            } else {
                "unknown file size".into()
            },
        };
    }

    // Very small files — no benefit from multi-segment.
    if input.total_bytes <= SINGLE_SEGMENT_THRESHOLD {
        return SegmentAdvice {
            segments: 1,
            reason: format!(
                "file size {} bytes <= {} threshold",
                input.total_bytes, SINGLE_SEGMENT_THRESHOLD
            ),
        };
    }

    let cpu_cores = available_parallelism();

    // How many segments the file can sustain (each >= MIN_BYTES_PER_SEGMENT).
    let by_size = (input.total_bytes / MIN_BYTES_PER_SEGMENT).max(1) as i32;

    // Downloads are I/O-bound (async waiting on network), not CPU-bound.
    // Allow up to 4× logical cores before capping — this lets a 4-core machine
    // use 16 segments and an 8-core machine use 32, which better saturates
    // high-bandwidth connections.
    let io_cap = (cpu_cores * 4).min(MAX_SEGMENTS);
    let recommended = by_size.min(io_cap).clamp(MIN_SEGMENTS, MAX_SEGMENTS);

    SegmentAdvice {
        segments: recommended,
        reason: format!(
            "file={} bytes, by_size={}, cpu_cores={}, io_cap={} → {}",
            input.total_bytes, by_size, cpu_cores, io_cap, recommended
        ),
    }
}

/// Calculate the optimal segment count **with** bandwidth estimation.
///
/// Refines `advise_static` by incorporating an observed download speed.
/// The `bandwidth_bps` is in **bytes per second**.
pub fn advise_with_bandwidth(input: &AdvisorInput, bandwidth_bps: f64) -> SegmentAdvice {
    let static_advice = advise_static(input);

    // If static already chose 1, don't override (server or file constraints).
    if static_advice.segments <= 1 {
        return static_advice;
    }

    // Scale the static recommendation based on bandwidth.
    //
    // Rationale:
    // - Very slow links (< 512 KB/s): multiple connections can bypass
    //   per-connection server-side throttling — keep at least 50% of the
    //   static recommendation (was 25%, too conservative).
    // - Medium links (512 KB/s – 5 MB/s): moderate parallelism helps.
    // - Fast links (5 MB/s – 50 MB/s): more segments can saturate the pipe.
    // - Very fast links (> 50 MB/s): full parallelism up to CPU limit.
    let bw_factor = if bandwidth_bps < BW_LOW {
        0.5 // keep ~50% of static recommendation (raised from 0.25)
    } else if bandwidth_bps < BW_MED {
        // Linear interpolation between 0.5 and 0.75
        let t = (bandwidth_bps - BW_LOW) / (BW_MED - BW_LOW);
        0.5 + t * 0.25
    } else if bandwidth_bps < BW_HIGH {
        // Linear interpolation between 0.75 and 1.0
        let t = (bandwidth_bps - BW_MED) / (BW_HIGH - BW_MED);
        0.75 + t * 0.25
    } else {
        1.0 // full parallelism
    };

    let adjusted = ((static_advice.segments as f64) * bw_factor).ceil() as i32;
    let final_segments = adjusted.clamp(MIN_SEGMENTS, MAX_SEGMENTS);

    SegmentAdvice {
        segments: final_segments,
        reason: format!(
            "static={}, bandwidth={:.1} KB/s, bw_factor={:.2} → {}",
            static_advice.segments,
            bandwidth_bps / 1024.0,
            bw_factor,
            final_segments
        ),
    }
}

/// Run a short bandwidth probe by downloading a small chunk and measuring
/// throughput.  Returns bytes/sec, or `None` if the probe fails or is
/// cancelled.
///
/// This reuses the download URL so we measure the *actual* server's speed
/// (not some generic speed-test endpoint).
pub async fn probe_bandwidth(
    client: &Client,
    url: &str,
    supports_range: bool,
    cancel_token: &CancellationToken,
    cookies: &str,
    referrer: &str,
    extra_headers: &std::collections::HashMap<String, String>,
) -> Option<f64> {
    use futures_util::StreamExt;

    // If server doesn't support Range, we can still probe — just download
    // from the beginning and abort after PROBE_BYTES.
    let mut req = client.get(url);
    if supports_range {
        req = req.header("Range", format!("bytes=0-{}", PROBE_BYTES - 1));
    }
    if !cookies.is_empty() {
        req = req.header("Cookie", cookies);
    }
    // 设置 Referer 头，部分 CDN/防盗链服务器需要此字段
    if !referrer.is_empty() {
        req = req.header(reqwest::header::REFERER, referrer);
    }
    // 应用用户自定义请求头
    req = crate::downloader::apply_extra_headers(req, extra_headers);

    let start = std::time::Instant::now();

    let resp = tokio::select! {
        biased;
        _ = cancel_token.cancelled() => return None,
        r = req.timeout(PROBE_TIMEOUT).send() => {
            match r {
                Ok(r) if r.status().is_success() || r.status().as_u16() == 206 => r,
                _ => return None,
            }
        }
    };

    let mut stream = resp.bytes_stream();
    let mut total: u64 = 0;

    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => return None,
            c = stream.next() => c,
        };

        match chunk {
            Some(Ok(bytes)) => {
                total += bytes.len() as u64;
                if total >= PROBE_BYTES {
                    break;
                }
            }
            Some(Err(_)) => break, // partial data is fine
            None => break,         // stream ended
        }
    }

    let elapsed = start.elapsed();
    if elapsed.as_millis() < 50 || total < 1024 {
        // Too fast or too little data to be meaningful.
        return None;
    }

    Some(total as f64 / elapsed.as_secs_f64())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get the number of logical CPU cores (falls back to 4 on error).
fn available_parallelism() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{AdvisorInput, advise_static, advise_with_bandwidth, probe_bandwidth};

    #[test]
    fn small_file_gets_one_segment() {
        let advice = advise_static(&AdvisorInput {
            total_bytes: 1_000_000, // 1 MB
            supports_range: true,
        });
        assert_eq!(advice.segments, 1);
    }

    #[test]
    fn no_range_support_gets_one_segment() {
        let advice = advise_static(&AdvisorInput {
            total_bytes: 1_000_000_000,
            supports_range: false,
        });
        assert_eq!(advice.segments, 1);
    }

    #[test]
    fn large_file_scales_up() {
        let advice = advise_static(&AdvisorInput {
            total_bytes: 1_000_000_000, // 1 GB
            supports_range: true,
        });
        assert!(advice.segments > 1);
        assert!(advice.segments <= 64);
    }

    #[test]
    fn slow_bandwidth_reduces_segments() {
        let input = AdvisorInput {
            total_bytes: 1_000_000_000,
            supports_range: true,
        };
        let slow = advise_with_bandwidth(&input, 100.0 * 1024.0); // 100 KB/s
        let fast = advise_with_bandwidth(&input, 100.0 * 1024.0 * 1024.0); // 100 MB/s
        assert!(slow.segments <= fast.segments);
    }

    #[test]
    fn unknown_size_gets_one_segment() {
        let advice = advise_static(&AdvisorInput {
            total_bytes: 0,
            supports_range: true,
        });
        assert_eq!(advice.segments, 1);
    }

    #[tokio::test]
    async fn probe_bandwidth_signature_accepts_referrer_and_extra_headers() {
        // 编译时验证：probe_bandwidth 接受 referrer 和 extra_headers 参数。
        // 使用一个不可达的调用点让编译器检查参数类型，无需真实 HTTP 服务器。
        if false {
            let client = reqwest::Client::new();
            let cancel = tokio_util::sync::CancellationToken::new();
            let headers = std::collections::HashMap::<String, String>::new();
            let _ = probe_bandwidth(
                &client, "http://x", true, &cancel, "cookie", "referer", &headers,
            )
            .await;
        }
    }
}
