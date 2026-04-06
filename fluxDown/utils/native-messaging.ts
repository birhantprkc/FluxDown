/**
 * Native Messaging 通信模块
 * 通过 Native Messaging 协议与 FluxDown 桌面应用通信
 *
 * 通信链路：
 *   Browser Extension
 *     <-> browser.runtime.connectNative() (stdin/stdout, 4字节LE长度前缀+JSON)
 *   fluxdown_nmh.exe (中继进程)
 *     <-> Named Pipe \\.\pipe\fluxdown (4字节LE长度前缀+JSON)
 *   FluxDown App
 *
 * 设计决策：
 *   - 使用 connectNative() 持久连接，复用同一 NMH 进程
 *   - 请求-响应通过 msg_id 匹配（递增 ID + pending Map）
 *   - App 未运行时由 NMH 自动启动，扩展端无需关心唤起逻辑
 *   - 超时 12s（预留 NMH 启动 App 的等待时间）
 */

import { browser } from "wxt/browser";

const NMH_NAME = "com.fluxdown.nmh";

// 每请求超时时间（NMH 启动 App 最多需要 ~7.5s，预留充足余量）
const REQUEST_TIMEOUT_MS = 12000;

// ──────────────────────────────────────────────────────────────
// 类型定义
// ──────────────────────────────────────────────────────────────

export interface DownloadRequest {
  url: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string>;
  fileSize?: number;
  mimeType?: string;
}

export interface ApiResponse {
  success: boolean;
  message?: string;
  taskId?: string;
}

export interface BatchDownloadItem {
  url: string;
  filename?: string;
  referrer?: string;
  cookies?: string;
  headers?: Record<string, string>;
  fileSize?: number;
  mimeType?: string;
}

// ──────────────────────────────────────────────────────────────
// 内部状态
// ──────────────────────────────────────────────────────────────

let _port: chrome.runtime.Port | null = null;
let _nextMsgId = 1;

interface PendingRequest {
  resolve: (value: ApiResponse) => void;
  timer: ReturnType<typeof setTimeout>;
}

const _pendingRequests = new Map<number, PendingRequest>();

// ──────────────────────────────────────────────────────────────
// 端口管理
// ──────────────────────────────────────────────────────────────

function getPort(): chrome.runtime.Port | null {
  if (_port) return _port;

  try {
    _port = browser.runtime.connectNative(NMH_NAME);
  } catch (e) {
    // connectNative() throws synchronously if the API is unavailable (e.g. permission denied).
    console.error("[FluxDown NMH] connectNative() threw:", e);
    return null;
  }

  _port.onMessage.addListener((msg: any) => {
    const msgId = msg?.msg_id;
    if (msgId == null) return;

    const pending = _pendingRequests.get(msgId);
    if (!pending) return;

    _pendingRequests.delete(msgId);
    clearTimeout(pending.timer);

    pending.resolve({
      success: msg.success ?? false,
      message: msg.message,
      taskId: msg.taskId,
    });
  });

  _port.onDisconnect.addListener((p) => {
    // Log disconnect reason to help diagnose NMH failures.
    // IMPORTANT: Firefox exposes the error on the port parameter p.error,
    // NOT on browser.runtime.lastError (which is always null in Firefox).
    // Chrome uses browser.runtime.lastError instead.
    // Common errors: "No such native application", "Access to the specified
    // native messaging host is forbidden" (extension ID mismatch).
    const err = (p as any).error ?? browser.runtime.lastError;
    if (err?.message) {
      console.error("[FluxDown NMH] port disconnected, reason:", err.message);
    } else {
      console.warn("[FluxDown NMH] port disconnected (no error reason)");
    }
    _port = null;
    // Reject all pending requests
    for (const [id, pending] of _pendingRequests) {
      clearTimeout(pending.timer);
      pending.resolve({ success: false, message: "port disconnected" });
      _pendingRequests.delete(id);
    }
  });

  return _port;
}

function disconnectPort() {
  if (_port) {
    try {
      _port.disconnect();
    } catch {
      /* ignore */
    }
    _port = null;
  }
}

// ──────────────────────────────────────────────────────────────
// 消息发送
// ──────────────────────────────────────────────────────────────

function sendMessage(
  action: string,
  payload: Record<string, any> = {},
): Promise<ApiResponse> {
  return new Promise<ApiResponse>((resolve) => {
    const port = getPort();
    if (!port) {
      resolve({ success: false, message: "native_messaging_unavailable" });
      return;
    }

    const msgId = _nextMsgId++;
    const timer = setTimeout(() => {
      _pendingRequests.delete(msgId);
      resolve({ success: false, message: "timeout" });
    }, REQUEST_TIMEOUT_MS);

    _pendingRequests.set(msgId, { resolve, timer });

    try {
      port.postMessage({ action, msg_id: msgId, ...payload });
    } catch {
      _pendingRequests.delete(msgId);
      clearTimeout(timer);
      disconnectPort();
      resolve({ success: false, message: "postMessage failed" });
    }
  });
}

/**
 * Send a message with one retry on transient failures.
 * If the first attempt fails due to a stale port or the App not running,
 * disconnects the old port and retries once — Chrome will spawn a fresh
 * NMH process which auto-launches the App.
 */
async function sendWithRetry(
  action: string,
  payload: Record<string, any>,
): Promise<ApiResponse> {
  const result = await sendMessage(action, payload);
  if (result.success) return result;

  // Retry once on transient failures (gets a fresh NMH process that auto-launches App)
  const retryable =
    result.message === "port disconnected" ||
    result.message === "postMessage failed" ||
    result.message === "app_not_running" ||
    result.message === "timeout";

  if (!retryable) return result;

  disconnectPort();

  // "port disconnected" 特殊处理：NMH 进程可能在将消息转发给 App 后才断开，
  // 此时消息已送达但响应丢失。重连后先 ping：如果 App 在线，说明消息
  // 大概率已送达，直接返回成功，避免重复发送导致 App 创建重复任务。
  if (result.message === "port disconnected" && action !== "ping") {
    const pingResult = await sendMessage("ping");
    if (pingResult.success) {
      console.log(
        "[FluxDown NMH] App alive after port disconnect — message likely delivered, skipping retry",
      );
      return {
        success: true,
        message: "delivered (reconnected after disconnect)",
      };
    }
    // ping 也失败，断开后重试发送原消息
    disconnectPort();
  }

  return sendMessage(action, payload);
}

// ──────────────────────────────────────────────────────────────
// 导出接口（与原 HTTP 版本完全兼容）
// ──────────────────────────────────────────────────────────────

export async function sendDownloadRequest(
  request: DownloadRequest,
): Promise<ApiResponse> {
  return sendWithRetry("download", request as Record<string, any>);
}

export async function sendBatchDownloadRequest(
  items: BatchDownloadItem[],
): Promise<ApiResponse> {
  if (items.length === 0) {
    return { success: false, message: "No items" };
  }

  // Send each item as an individual download request to preserve per-item
  // cookies, headers, referrer, fileSize, and mimeType.  The Rust NMH
  // protocol only supports single-item DownloadRequest — batching by
  // newline-joining URLs discards all per-item auth metadata.
  const results = await Promise.allSettled(
    items.map((item) =>
      sendDownloadRequest({
        url: item.url,
        filename: item.filename || "",
        referrer: item.referrer || "",
        cookies: item.cookies,
        headers: item.headers,
        fileSize: item.fileSize,
        mimeType: item.mimeType,
      }),
    ),
  );

  const succeeded = results.filter(
    (r) => r.status === "fulfilled" && r.value.success,
  ).length;
  const failed = results.length - succeeded;

  if (succeeded === 0) {
    const firstError = results.find(
      (r) =>
        (r.status === "fulfilled" && !r.value.success) ||
        r.status === "rejected",
    );
    const message =
      firstError?.status === "fulfilled"
        ? firstError.value.message
        : firstError?.status === "rejected"
          ? String(firstError.reason)
          : "All items failed";
    return { success: false, message: `Batch failed: ${message}` };
  }

  return {
    success: true,
    message:
      failed > 0
        ? `${succeeded}/${results.length} items sent (${failed} failed)`
        : `${succeeded} items sent`,
  };
}

export async function checkFluxDownAvailable(): Promise<boolean> {
  const result = await sendMessage("ping");
  return result.success === true;
}
