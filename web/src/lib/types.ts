// Wire 契约 —— 与 native/api `types.rs` + native/server `wire.rs` 一一对应（camelCase）。

/** 任务状态码：0=pending 1=downloading 2=paused 3=completed 4=error 5=preparing */
export type TaskStatus = 0 | 1 | 2 | 3 | 4 | 5

export interface TaskDto {
  taskId: string
  url: string
  fileName: string
  saveDir: string
  status: TaskStatus
  downloadedBytes: number
  totalBytes: number
  errorMessage: string
  /** Unix 秒时间戳（字符串） */
  createdAt: string
  proxyUrl: string
  queueId: string
  checksum: string
  /** 文件跟踪：completed 任务的目标文件是否已丢失（被删除/移动）。默认 false */
  fileMissing?: boolean
}

export interface QueueDto {
  queueId: string
  name: string
  speedLimitKbps: number
  maxConcurrent: number
  defaultSaveDir: string
  position: number
  defaultSegments: number
  defaultUserAgent: string
}

export interface CreateTaskRequest {
  url: string
  fileName?: string
  saveDir?: string
  segments?: number
  cookies?: string
  referrer?: string
  proxyUrl?: string
  userAgent?: string
  queueId?: string
  checksum?: string
  headers?: Record<string, string>
}

export interface CreatedTask {
  taskId: string
}

export interface ApiInfo {
  app: string
  version: string
}

export interface SegmentDetail {
  index: number
  startByte: number
  endByte: number
  downloadedBytes: number
}

export interface HlsQualityOption {
  index: number
  bandwidth: number
  width: number
  height: number
}

export interface BtFileEntry {
  index: number
  path: string
  size: number
}

// ---- WS 服务端 → 客户端（tag = type） ----

export type WsServerMsg =
  | ({ type: 'taskProgress' } & TaskProgressMsg)
  | { type: 'tasksSnapshot'; tasks: TaskDto[] }
  | ({ type: 'segmentProgress' } & SegmentProgressMsg)
  | ({ type: 'segmentSplit' } & SegmentSplitMsg)
  | { type: 'taskMetaProbed'; taskId: string; fileName: string; totalBytes: number }
  | { type: 'queuesChanged'; queues: QueueDto[] }
  | { type: 'queuePositionsChanged'; positions: { taskId: string; position: number }[] }
  | { type: 'priorityTaskChanged'; priorityTaskId: string; autoPausedCount: number }
  | { type: 'hlsSelectionRequest'; taskId: string; options: HlsQualityOption[] }
  | { type: 'btSelectionRequest'; taskId: string; files: BtFileEntry[] }
  | { type: 'pong' }

export interface TaskProgressMsg {
  taskId: string
  status: TaskStatus
  downloadedBytes: number
  totalBytes: number
  speed: number
  fileName: string
  saveDir: string
  url: string
  errorMessage: string
}

export interface SegmentProgressMsg {
  taskId: string
  totalBytes: number
  segmentCount: number
  segments: SegmentDetail[]
}

export interface SegmentSplitMsg {
  taskId: string
  parentIndex: number
  parentNewEnd: number
  childIndex: number
  childStart: number
  childEnd: number
  isProactive: boolean
  totalSegments: number
}

// ---- WS 客户端 → 服务端 ----

export type WsClientMsg =
  | { type: 'hlsSelection'; taskId: string; selectedIndex: number }
  | { type: 'btSelection'; taskId: string; selectedIndices: number[] }
  | { type: 'ping' }

// ---- 扩展 REST ----

export interface ProxyTestRequest {
  proxyType: string
  host: string
  port: string
  username?: string
  password?: string
}

export interface ProxyTestResponse {
  latencyMs: number
}

export interface CreateQueueRequest {
  name: string
  speedLimitKbps?: number
  maxConcurrent?: number
  defaultSaveDir?: string
  defaultSegments?: number
  defaultUserAgent?: string
}

export interface FsEntry {
  name: string
  path: string
}

export interface FsListResponse {
  path: string
  parent: string | null
  dirs: FsEntry[]
}

export interface StatsResponse {
  diskFreeBytes: number | null
  saveDir: string
  serverVersion: string
  wsClients: number
  /** 演示模式开关（服务器以 FLUXDOWN_DEMO_URL 启动时为 true）。 */
  demoMode: boolean
  /** 演示模式下唯一允许下载的 URL；非演示模式为空串。 */
  demoUrl: string
}

export interface TokenResponse {
  token: string
  note: string
}

export type ConfigMap = Record<string, string>
