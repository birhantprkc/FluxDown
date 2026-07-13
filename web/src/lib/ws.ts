// WebSocket 实时通道：可重连、按 type 分派到轻量外部 store + Query 缓存。
//
// live 数据（speed/进度/分段）不进 React Query —— 高频更新走
// useSyncExternalStore 的细粒度订阅；任务/队列列表本体在 Query 缓存
// （['tasks'] / ['queues']），由 tasksSnapshot / queuesChanged 直接 setQueryData。

import { useSyncExternalStore } from 'react'
import type { QueryClient } from '@tanstack/react-query'
import { getBase, getToken, isAuthenticated } from './auth'
import { t } from './i18n'
import type {
  BtFileEntry,
  HlsQualityOption,
  QueueDto,
  SegmentProgressMsg,
  SegmentSplitMsg,
  TaskDto,
  TaskProgressMsg,
  WsClientMsg,
  WsServerMsg,
} from './types'

// ---------------- 轻量外部 store ----------------

export class Store<T> {
  private listeners = new Set<() => void>()
  private state: T
  constructor(initial: T) {
    this.state = initial
  }
  get = (): T => this.state
  set = (next: T | ((prev: T) => T)) => {
    this.state = typeof next === 'function' ? (next as (prev: T) => T)(this.state) : next
    for (const l of this.listeners) l()
  }
  subscribe = (cb: () => void) => {
    this.listeners.add(cb)
    return () => this.listeners.delete(cb)
  }
}

export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.get, store.get)
}

// ---------------- store 实例 ----------------

export type TaskLive = Omit<TaskProgressMsg, 'taskId'>

export const liveStore = new Store<Record<string, TaskLive>>({})
export const segmentStore = new Store<Record<string, SegmentProgressMsg>>({})
/** 最近一次拆分事件（详情面板播放拆分动画用），带到达时间戳。 */
export const splitStore = new Store<(SegmentSplitMsg & { at: number }) | null>(null)
export const connStore = new Store<{
  status: 'connecting' | 'connected' | 'disconnected'
  rttMs: number | null
}>({ status: 'disconnected', rttMs: null })
export const priorityStore = new Store<{ priorityTaskId: string; autoPausedCount: number }>({
  priorityTaskId: '',
  autoPausedCount: 0,
})
/** 待处理的 HLS/BT 选择请求（对话框消费后置 null）。 */
export const hlsRequestStore = new Store<{ taskId: string; options: HlsQualityOption[] } | null>(null)
export const btRequestStore = new Store<{ taskId: string; files: BtFileEntry[] } | null>(null)

// ---------------- 连接管理 ----------------

let socket: WebSocket | null = null
let reconnectTimer: ReturnType<typeof setTimeout> | null = null
let pingTimer: ReturnType<typeof setInterval> | null = null
let pingSentAt = 0
let attempts = 0
let queryClientRef: QueryClient | null = null

export function sendWs(msg: WsClientMsg) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(msg))
}

export function disconnectWs() {
  if (reconnectTimer) clearTimeout(reconnectTimer)
  if (pingTimer) clearInterval(pingTimer)
  reconnectTimer = null
  pingTimer = null
  socket?.close()
  socket = null
  connStore.set({ status: 'disconnected', rttMs: null })
}

export function connectWs(queryClient: QueryClient) {
  queryClientRef = queryClient
  if (socket && socket.readyState <= WebSocket.OPEN) return
  if (!isAuthenticated()) return
  openSocket()
}

function wsUrl(): string {
  const base = getBase()
  const origin = base || location.origin
  const url = new URL(origin)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  url.pathname = '/api/v1/ws'
  url.search = `?token=${encodeURIComponent(getToken())}`
  return url.toString()
}

function openSocket() {
  connStore.set((s) => ({ ...s, status: 'connecting' }))
  const ws = new WebSocket(wsUrl())
  socket = ws

  ws.onopen = () => {
    attempts = 0
    connStore.set({ status: 'connected', rttMs: null })
    if (pingTimer) clearInterval(pingTimer)
    pingTimer = setInterval(() => {
      pingSentAt = performance.now()
      sendWs({ type: 'ping' })
    }, 15_000)
    // 立即测一次 RTT
    pingSentAt = performance.now()
    sendWs({ type: 'ping' })
  }

  ws.onmessage = (e) => {
    let msg: WsServerMsg
    try {
      msg = JSON.parse(e.data as string) as WsServerMsg
    } catch {
      return
    }
    dispatch(msg)
  }

  ws.onclose = () => {
    if (socket !== ws) return
    socket = null
    if (pingTimer) clearInterval(pingTimer)
    connStore.set({ status: 'disconnected', rttMs: null })
    // 指数退避重连（1s → 2s → 4s … 上限 15s）；登出后不再重连。
    if (!isAuthenticated()) return
    const delay = Math.min(1000 * 2 ** attempts, 15_000)
    attempts += 1
    reconnectTimer = setTimeout(openSocket, delay)
  }

  ws.onerror = () => {
    ws.close()
  }
}

function dispatch(msg: WsServerMsg) {
  const qc = queryClientRef
  switch (msg.type) {
    case 'taskProgress': {
      const { taskId, ...live } = msg
      liveStore.set((prev) => ({ ...prev, [taskId]: live }))
      if (qc) {
        const tasks = qc.getQueryData<TaskDto[]>(['tasks'])
        if (!tasks || !tasks.some((t) => t.taskId === taskId)) {
          // 新任务（其他客户端/aria2 创建）→ 拉全量。
          void qc.invalidateQueries({ queryKey: ['tasks'] })
        } else {
          qc.setQueryData<TaskDto[]>(['tasks'], (old) =>
            old?.map((t) =>
              t.taskId === taskId
                ? {
                    ...t,
                    status: live.status,
                    downloadedBytes: live.downloadedBytes,
                    totalBytes: live.totalBytes || t.totalBytes,
                    fileName: live.fileName || t.fileName,
                    errorMessage: live.errorMessage,
                  }
                : t,
            ),
          )
        }
      }
      break
    }
    case 'tasksSnapshot':
      queryClientRef?.setQueryData<TaskDto[]>(['tasks'], msg.tasks)
      break
    case 'segmentProgress':
      segmentStore.set((prev) => ({ ...prev, [msg.taskId]: msg }))
      break
    case 'segmentSplit':
      splitStore.set({ ...msg, at: Date.now() })
      break
    case 'taskMetaProbed':
      queryClientRef?.setQueryData<TaskDto[]>(['tasks'], (old) =>
        old?.map((t) =>
          t.taskId === msg.taskId
            ? { ...t, fileName: msg.fileName || t.fileName, totalBytes: msg.totalBytes || t.totalBytes }
            : t,
        ),
      )
      break
    case 'queuesChanged':
      queryClientRef?.setQueryData<QueueDto[]>(['queues'], msg.queues)
      break
    case 'queuePositionsChanged':
      // 位置信息暂不驱动 UI（列表按时间分组），忽略。
      break
    case 'priorityTaskChanged':
      priorityStore.set({ priorityTaskId: msg.priorityTaskId, autoPausedCount: msg.autoPausedCount })
      break
    case 'hlsSelectionRequest':
      hlsRequestStore.set({ taskId: msg.taskId, options: msg.options })
      break
    case 'btSelectionRequest':
      btRequestStore.set({ taskId: msg.taskId, files: msg.files })
      break
    case 'pluginsChanged':
      void queryClientRef?.invalidateQueries({ queryKey: ['plugins'] })
      break
    case 'pluginAutoDisabled': {
      const identity = msg.identity
      // 动态 import 打破与 confirm.ts 的静态循环依赖（confirm.ts 反向依赖本模块的 Store）。
      void import('./confirm').then(({ alertDialog }) =>
        alertDialog({
          title: t('plugins.autoDisabledTitle'),
          message: t('plugins.autoDisabledMsg', { name: identity }),
        }),
      )
      break
    }
    case 'pong':
      connStore.set({ status: 'connected', rttMs: Math.round(performance.now() - pingSentAt) })
      break
  }
}

// ---------------- 派生 hooks ----------------

/** 全局下载速度（所有 downloading 任务 live speed 之和）。 */
export function useGlobalSpeed(): number {
  const live = useStore(liveStore)
  let sum = 0
  for (const v of Object.values(live)) if (v.status === 1) sum += v.speed
  return sum
}
