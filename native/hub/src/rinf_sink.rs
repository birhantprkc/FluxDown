//! `EventSink` 实现 —— 把 `EngineEvent` 变体转发为具体 Dart 信号。
//!
//! 这是现状 hub 内 22 处 `.send_signal_to_dart()` 调用点的收敛,内容是搬移
//! 而非新写业务逻辑。

use fluxdown_engine::events::{EngineEvent, EventSink};
use rinf::RustSignal;

use crate::signals;

/// 桥接 `EngineEvent` 到 `hub::signals::*` 具体信号类型的 `EventSink` 实现。
pub struct RinfEventSink;

impl EventSink for RinfEventSink {
    fn emit(&self, event: EngineEvent) {
        match event {
            EngineEvent::TaskProgress {
                task_id,
                status,
                downloaded_bytes,
                total_bytes,
                speed,
                file_name,
                save_dir,
                url,
                error_message,
            } => {
                signals::TaskProgress {
                    task_id,
                    status,
                    downloaded_bytes,
                    total_bytes,
                    speed,
                    file_name,
                    save_dir,
                    url,
                    error_message,
                }
                .send_signal_to_dart();
            }
            EngineEvent::TasksSnapshot(tasks) => {
                signals::AllTasks {
                    tasks: tasks.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::SegmentProgress {
                task_id,
                total_bytes,
                segment_count,
                segments,
            } => {
                signals::SegmentProgress {
                    task_id,
                    total_bytes,
                    segment_count,
                    segments: segments.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::TaskMetaProbed {
                task_id,
                file_name,
                total_bytes,
            } => {
                signals::TaskMetaProbed {
                    task_id,
                    file_name,
                    total_bytes,
                }
                .send_signal_to_dart();
            }
            EngineEvent::QueuePositionsChanged(positions) => {
                signals::QueuePositionsUpdate {
                    positions: positions.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::QueuesChanged(queues) => {
                signals::AllQueues {
                    queues: queues.into_iter().map(Into::into).collect(),
                }
                .send_signal_to_dart();
            }
            EngineEvent::PriorityTaskChanged {
                priority_task_id,
                auto_paused_count,
            } => {
                signals::PriorityTaskChanged {
                    priority_task_id,
                    auto_paused_count,
                }
                .send_signal_to_dart();
            }
            EngineEvent::SegmentSplit {
                task_id,
                parent_index,
                parent_new_end,
                child_index,
                child_start,
                child_end,
                is_proactive,
                total_segments,
            } => {
                signals::SegmentSplitEvent {
                    task_id,
                    parent_index,
                    parent_new_end,
                    child_index,
                    child_start,
                    child_end,
                    is_proactive,
                    total_segments,
                }
                .send_signal_to_dart();
            }
            EngineEvent::FileMissingChanged(updates) => {
                signals::FileMissingChanged {
                    updates: updates
                        .into_iter()
                        .map(|(task_id, missing)| signals::FileMissingUpdate { task_id, missing })
                        .collect(),
                }
                .send_signal_to_dart();
            }
            // `#[non_exhaustive]`：未来新增变体默认丢弃并记录日志，而非编译失败。
            _ => {
                crate::logger::log_info!(
                    "[rinf-sink] unhandled EngineEvent variant (added after this match was written)"
                );
            }
        }
    }
}
