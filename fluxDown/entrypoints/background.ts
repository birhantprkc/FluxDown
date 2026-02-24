/**
 * FluxDown Background Service Worker
 *
 * 职责：
 * 1. 拦截浏览器下载事件，转发给 FluxDown 桌面应用
 * 2. 注册右键菜单（发送链接到 FluxDown）
 * 3. 管理与 Native Host 的通信
 * 4. 响应 popup 的消息
 * 5. 维护拦截统计数据
 * 6. 多语言支持
 *
 * === 下载拦截三层防线 ===
 *
 * 第一层（HTTP 响应感知）: webRequest.onHeadersReceived
 *   - 监听所有请求的响应头
 *   - 当响应包含 Content-Disposition: attachment 或 下载类 Content-Type 时，
 *     将该 URL 标记为"已知下载"，缓存 Content-Type / Content-Length / 文件名等
 *   - 为后续 onCreated 兜底提供可靠的元数据来源
 *
 * 第二层（主拦截）: downloads.onDeterminingFilename
 *   - 浏览器弹出「另存为」之前触发，suggest({ cancel: true }) 可取消下载
 *   - 最优先、最干净的拦截方式
 *   - 但对 JS location.href / meta refresh 触发的"导航转下载"存在 MV3 时序问题
 *
 * 第三层（兜底拦截）: downloads.onCreated + onChanged
 *   - onCreated 始终可靠触发，配合 onChanged 等待元数据就绪后再判断
 *   - 如果 onDeterminingFilename 在限定时间内未处理，由此层接管
 *   - 利用第一层缓存的 HTTP 响应信息来补全 downloadItem 中缺失的元数据
 */

import { sendDownloadRequest, sendBatchDownloadRequest, checkFluxDownAvailable } from '@/utils/native-messaging';
import type { DownloadRequest, BatchDownloadItem } from '@/utils/native-messaging';
import { loadSettings, shouldIntercept } from '@/utils/settings';
import type { DownloadItemInfo } from '@/utils/settings';
import { initI18n, t } from '@/utils/i18n';
import { isSniffableContentType, classifyResource, extractFilenameFromUrl } from '@/utils/resource-types';
import type { ResourceMessagePayload } from '@/utils/resource-types';
import {
  addResources,
  addSniffedResource,
  getResourcesForTab,
  getResourceCountForTab,
  clearResourcesForTab,
  updateBadgeForTab,
  initTabLifecycleListeners,
} from '@/utils/resource-store';

// ===== 统计相关 =====
interface DailyStats {
  sent: number;
  failed: number;
  date: string;
}

// Bug 4 修复：用序列化 Promise 链防止 incrementStat 并发读写竞态
// chrome.storage 不提供事务，多个并发 get→modify→set 会导致写入丢失
let _statChain: Promise<void> = Promise.resolve();

function incrementStat(field: 'sent' | 'failed'): Promise<void> {
  _statChain = _statChain.then(async () => {
    try {
      const today = new Date().toDateString();
      const result = await browser.storage.local.get('stats');
      let stats: DailyStats = result.stats || { sent: 0, failed: 0, date: '' };
      if (stats.date !== today) {
        stats = { sent: 0, failed: 0, date: today };
      }
      stats[field]++;
      await browser.storage.local.set({ stats });
    } catch { /* storage 失败不影响主流程 */ }
  });
  return _statChain;
}

export default defineBackground(() => {
  console.log('[FluxDown] Background service worker started');

  // ===== P3: settings 内存缓存 =====
  // 避免每次拦截都 await chrome.storage.sync.get（热路径去异步化）
  // storage.onChanged 保证跨标签页/窗口的实时同步
  let _settingsCache: import('@/utils/settings').FluxDownSettings | null = null;
  let _settingsCacheTs = 0;
  const SETTINGS_CACHE_TTL = 2000; // 2 秒，足够应对快速连续下载
  // Bug 8 修复：用 inflight Promise 防止并发调用时发起多次 loadSettings
  let _settingsInflight: Promise<import('@/utils/settings').FluxDownSettings> | null = null;
  // Bug R3-1/R3-9 修复：用版本序号防止 inflight 竞态
  // storage.onChanged 可能在 inflight 期间触发，导致旧值写回覆盖新设置。
  // 每次 storage 变化时递增版本号，inflight 完成时检查版本是否已变，已变则丢弃。
  let _settingsVersion = 0;

  async function getCachedSettings() {
    const now = Date.now();
    if (_settingsCache && now - _settingsCacheTs < SETTINGS_CACHE_TTL) {
      return _settingsCache;
    }
    // 已有 inflight 请求则复用，避免并发发起多次 storage.sync.get
    if (_settingsInflight) return _settingsInflight;
    const versionAtStart = _settingsVersion;
    _settingsInflight = loadSettings().then((s) => {
      // 版本已变说明 storage.onChanged 在 loadSettings() 期间触发，
      // 当前结果是旧版本的设置，丢弃缓存写入并强制下次重新加载
      if (_settingsVersion === versionAtStart) {
        _settingsCache = s;
        _settingsCacheTs = Date.now();
      }
      _settingsInflight = null;
      return s;
    }).catch((e) => {
      _settingsInflight = null;
      throw e;
    });
    return _settingsInflight;
  }

  // 监听 storage 变化，立即失效缓存
  browser.storage.onChanged.addListener((changes, area) => {
    if (area === 'sync' && changes.settings) {
      _settingsCache = null;
      _settingsCacheTs = 0;
      _settingsInflight = null;
      _settingsVersion++; // 使任何进行中的 inflight 结果失效
    }
  });

  // 启动时预热缓存
  // R5-8 修复：加 .catch 防止 loadSettings 失败产生未捕获 rejection 警告
  getCachedSettings().then((s) => {
    updateIcon(s.enabled);
    console.log('[FluxDown] Settings cache warmed up');
  }).catch((e) => {
    console.warn('[FluxDown] Settings cache warmup failed (non-fatal):', e);
  });

  // 初始化 i18n
  // R7-2 修复：加 .catch() 防止意外异常成为未捕获 rejection 噪音
  initI18n().then(() => {
    console.log('[FluxDown] i18n initialized');
  }).catch((e) => {
    console.warn('[FluxDown] i18n init failed (non-fatal):', e);
  });

  // 初始化 tab 生命周期监听器（自动清理关闭/导航的 tab 资源）
  initTabLifecycleListeners();

  // ==========================================
  // 第一层：HTTP 响应感知（webRequest 缓存）
  // ==========================================

  // 请求头缓存（Cookie / Authorization）
  const requestHeaderCache = new Map<string, { cookies: string; headers: Record<string, string>; ts: number }>();

  // 响应头缓存 —— 当 HTTP 响应指示"这是一个下载"时，缓存其元数据
  // 这是第三层兜底拦截的关键数据来源
  interface ResponseDownloadInfo {
    url: string;
    contentType: string;         // Content-Type
    contentLength: number;       // Content-Length（-1 = 未知）
    dispositionFilename: string; // 从 Content-Disposition 解析出的文件名
    ts: number;
  }
  const responseDownloadCache = new Map<string, ResponseDownloadInfo>();

  // Chrome MV3 需要 'extraHeaders' 才能看到 Cookie 等敏感头，Firefox 不需要也不识别此选项
  const sendHeadersOpts: string[] = ['requestHeaders'];
  try {
    // 先尝试带 extraHeaders（Chrome MV3），失败则降级（Firefox）
    browser.webRequest.onSendHeaders.addListener(
      onSendHeadersHandler,
      { urls: ['<all_urls>'] },
      [...sendHeadersOpts, 'extraHeaders'] as any,
    );
    console.log('[FluxDown] webRequest.onSendHeaders listener registered (with extraHeaders)');
  } catch {
    try {
      browser.webRequest.onSendHeaders.addListener(
        onSendHeadersHandler,
        { urls: ['<all_urls>'] },
        sendHeadersOpts,
      );
      console.log('[FluxDown] webRequest.onSendHeaders listener registered (without extraHeaders)');
    } catch (e) {
      console.warn('[FluxDown] Failed to register webRequest.onSendHeaders listener:', e);
    }
  }

  // Bug R2-8 修复：将 requestHeaderCache 清理从"每次请求时全量遍历"改为周期性清理。
  // 高流量页面（如视频网站）每秒可能触发数百次 onSendHeaders，之前每次都 O(n) 遍历会造成性能问题。
  setInterval(() => {
    const now = Date.now();
    for (const [cachedUrl, entry] of requestHeaderCache) {
      if (now - entry.ts > 60_000) {
        requestHeaderCache.delete(cachedUrl);
      }
    }
    // 强制大小上限（防止突发积累过多条目）
    if (requestHeaderCache.size > 1000) {
      const excess = requestHeaderCache.size - 800;
      let deleted = 0;
      for (const key of requestHeaderCache.keys()) {
        if (deleted >= excess) break;
        requestHeaderCache.delete(key);
        deleted++;
      }
    }
  }, 30_000); // 每 30 秒清理一次，远低于 60 秒有效期

  function onSendHeadersHandler(details: chrome.webRequest.WebRequestHeadersDetails) {
    if (!details.requestHeaders) return;
    const headers: Record<string, string> = {};
    let cookies = '';
    for (const h of details.requestHeaders) {
      if (h.name && h.value) {
        headers[h.name] = h.value;
        if (h.name.toLowerCase() === 'cookie') {
          cookies = h.value;
        }
      }
    }
    requestHeaderCache.set(details.url, { cookies, headers, ts: Date.now() });
  }

  // === 响应头监听：检测"导航转下载"场景 ===
  // 当浏览器主框架导航的响应带有 Content-Disposition: attachment 或
  // 下载类 Content-Type 时，说明这是一个"导航转下载"的请求。
  // 缓存其信息，供 onCreated 兜底拦截使用。
  try {
    browser.webRequest.onHeadersReceived.addListener(
      (details) => {
        // 只关注主框架导航（sub_frame、xhr 等交给正常 download 流程处理）
        if (details.type !== 'main_frame') return;
        if (!details.responseHeaders) return;

        let contentType = '';
        let contentLength = -1;
        let contentDisposition = '';

        for (const h of details.responseHeaders) {
          const name = h.name.toLowerCase();
          if (name === 'content-type' && h.value) {
            contentType = h.value.split(';')[0].trim().toLowerCase();
          } else if (name === 'content-length' && h.value) {
            const parsed = parseInt(h.value, 10);
            if (!isNaN(parsed)) contentLength = parsed;
          } else if (name === 'content-disposition' && h.value) {
            contentDisposition = h.value;
          }
        }

        // 判断该响应是否会触发下载
        const isAttachment = contentDisposition.toLowerCase().startsWith('attachment');
        const isDownloadMime = isDownloadContentType(contentType);

        if (!isAttachment && !isDownloadMime) return;

        // 从 Content-Disposition 提取文件名
        const dispositionFilename = parseContentDispositionFilename(contentDisposition);

        const info: ResponseDownloadInfo = {
          url: details.url,
          contentType,
          contentLength,
          dispositionFilename,
          ts: Date.now(),
        };

        responseDownloadCache.set(details.url, info);
        console.log('[FluxDown] Detected download-triggering response (onHeadersReceived):', info);

        // 60 秒后自动清理
        setTimeout(() => responseDownloadCache.delete(details.url), 60_000);

        // 同时将 main_frame 下载资源加入嗅探面板
        // （资源嗅探层只监听 media/xhr/object/other，main_frame 会绕过它）
        if (details.tabId >= 0) {
          const added = addSniffedResource(
            details.tabId,
            details.url,
            contentType,
            contentLength,
            dispositionFilename,
            isAttachment,
          );
          if (added > 0) {
            updateBadgeForTab(details.tabId);
            notifyContentScript(details.tabId);
          }
        }
      },
      { urls: ['<all_urls>'] },
      ['responseHeaders'],
    );
    console.log('[FluxDown] webRequest.onHeadersReceived listener registered');
  } catch (e) {
    console.warn('[FluxDown] Failed to register webRequest.onHeadersReceived listener:', e);
  }

  // ==========================================
  // 资源嗅探层：监听所有 media / XHR 类型请求的响应头
  // 检测可下载的媒体资源，加入资源列表供 UI 展示
  // ==========================================
  try {
    browser.webRequest.onHeadersReceived.addListener(
      (details) => {
        // 跳过无效或非 tab 请求
        if (details.tabId < 0 || !details.responseHeaders) return;

        // 跳过非成功响应（重定向、客户端/服务器错误）
        if (details.statusCode < 200 || details.statusCode >= 400) return;

        let contentType = '';
        let contentLength = -1;
        let contentDisposition = '';

        for (const h of details.responseHeaders) {
          const name = h.name.toLowerCase();
          if (name === 'content-type' && h.value) {
            contentType = h.value.split(';')[0].trim().toLowerCase();
          } else if (name === 'content-length' && h.value) {
            const parsed = parseInt(h.value, 10);
            if (!isNaN(parsed)) contentLength = parsed;
          } else if (name === 'content-disposition' && h.value) {
            contentDisposition = h.value;
          }
        }

        // 判断是否是有价值的资源
        const isSniffable = isSniffableContentType(contentType);
        const isAttachment = contentDisposition.toLowerCase().startsWith('attachment');

        if (!isSniffable && !isAttachment) return;

        // 提取文件名
        let filename = '';
        if (contentDisposition) {
          filename = parseContentDispositionFilename(contentDisposition);
        }
        if (!filename) {
          filename = extractFilenameFromUrl(details.url);
        }

        // 添加到资源存储（传递 isAttachment 标记用于可信度计算）
        const added = addSniffedResource(
          details.tabId,
          details.url,
          contentType,
          contentLength,
          filename,
          isAttachment,
        );

        if (added > 0) {
          // 更新 Badge
          updateBadgeForTab(details.tabId);
          // 推送给 Content Script UI
          notifyContentScript(details.tabId);
        }
      },
      { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'object', 'other', 'sub_frame'] },
      ['responseHeaders'],
    );
    console.log('[FluxDown] Resource sniffer (onHeadersReceived for media) registered');
  } catch (e) {
    console.warn('[FluxDown] Failed to register resource sniffer:', e);
  }

  /**
   * 向指定 tab 的 Content Script 推送最新资源列表
   */
  async function notifyContentScript(tabId: number): Promise<void> {
    const resources = getResourcesForTab(tabId);
    try {
      await browser.tabs.sendMessage(tabId, {
        action: 'resourcesUpdated',
        resources,
      });
    } catch {
      // Content script 可能还未注入
    }
  }

  // ==========================================
  // 第二层 + 第三层：下载事件拦截
  // ==========================================

  const downloadItemCache = new Map<number, chrome.downloads.DownloadItem>();
  const handledDownloads = new Map<number, 'primary' | 'fallback'>();
  // Alt+Click 绕过令牌：URL → 过期时间戳，15 秒内有效
  const bypassTokens = new Map<string, number>();

  // Bug R3-2 修复：周期性清理 bypassTokens 中的过期条目，防止长期积累内存泄漏
  setInterval(() => {
    const now = Date.now();
    for (const [tokenUrl, expiry] of bypassTokens) {
      if (expiry <= now) bypassTokens.delete(tokenUrl);
    }
  }, 60_000); // 每 60 秒清理一次

  // Firefox 不支持 onDeterminingFilename，兜底层是唯一拦截路径，
  // 需要更长等待让浏览器填充 downloadItem 元数据
  const hasDeterminingFilename = !!browser.downloads.onDeterminingFilename;

  // ──────────────────────────────────────────────────────────────
  // 弱网可靠性：统一等待元数据就绪
  // ──────────────────────────────────────────────────────────────
  // 设计原则：
  //   - 路径A（main_frame 导航下载）：等待 responseDownloadCache 填充
  //   - 路径B（XHR / JS 触发下载）：等待 downloadItem.mime/filename 填充
  //   - 两条路径共享同一个 deadline，总等待上限为 META_MAX_WAIT（5s）
  //   - 两路并发轮询：responseCache 先到就走路径A，否则走路径B
  //
  // Bug 1 修复：之前路径A的 waitForResponseCache 耗尽 5s 后，路径B才开始
  //             等待，导致总等待 10s，且路径B内不再检查缓存（死路）。
  // Bug 6 修复：现在两路并发，5s 是总上限，不是每路各 5s。
  const POLL_INTERVAL = 60;       // 统一轮询间隔 ms
  const META_MAX_WAIT = 5000;     // 总等待上限（弱网场景覆盖范围）

  /**
   * 并发等待两种元数据来源，哪个先到用哪个，共享 deadline。
   * 返回：
   *   { source: 'responseCache', data: ResponseDownloadInfo } — main_frame 导航下载
   *   { source: 'downloadMeta', data: DownloadItem }          — XHR/JS 触发下载
   *   null — deadline 到达或下载已被其他层处理
   */
  type MetaResult =
    | { source: 'responseCache'; data: ResponseDownloadInfo }
    | { source: 'downloadMeta'; data: chrome.downloads.DownloadItem };

  async function waitForMeta(
    url: string,
    downloadId: number,
    originalItem: chrome.downloads.DownloadItem,
    deadlineMs: number,
  ): Promise<MetaResult | null> {
    let bestItem = originalItem;
    while (Date.now() < deadlineMs) {
      if (handledDownloads.has(downloadId)) return null;

      // 路径A：responseDownloadCache 到达
      const rc = responseDownloadCache.get(url);
      if (rc) return { source: 'responseCache', data: rc };

      // 路径B：downloadItem 元数据就绪
      try {
        const results = await browser.downloads.search({ id: downloadId });
        if (results && results.length > 0) {
          const item = results[0];
          if (item.state === 'complete' || (item as any).state === 'interrupted') {
            // 下载已结束（可能极快完成），用最新状态返回
            return { source: 'downloadMeta', data: item };
          }
          if (item.mime || item.filename) {
            return { source: 'downloadMeta', data: item };
          }
          bestItem = item;
        } else if (results && results.length === 0) {
          // Bug R4-4 修复：search 返回空数组说明该下载项已不存在（被第二层 erase，或已被删除）。
          // 视为已被其他路径处理，退出，避免误用过期的 originalItem 重复发送。
          return null;
        }
      } catch {
        // search 失败（Firefox 偶发），继续等待
      }

      await sleep(POLL_INTERVAL);
    }
    // deadline 到达，用 bestItem 兜底（宁可用不完整数据判断也不放弃拦截）
    // 再最后检查一次缓存
    const rc = responseDownloadCache.get(url);
    if (rc) return { source: 'responseCache', data: rc };
    return { source: 'downloadMeta', data: bestItem };
  }

  // === 第三层：onCreated 兜底 + onChanged 元数据补全 ===
  browser.downloads.onCreated.addListener((downloadItem) => {
    const downloadId = downloadItem.id;
    const url = downloadItem.url;

    console.log('[FluxDown] onCreated:', { id: downloadId, url, mime: downloadItem.mime, filename: downloadItem.filename });

    // 缓存 downloadItem 信息，onDeterminingFilename 会用到
    downloadItemCache.set(downloadId, downloadItem);

    // 跳过 blob 和 data URL
    if (url.startsWith('blob:') || url.startsWith('data:')) return;

    // 启动兜底计时器
    // 给 onDeterminingFilename 一个处理窗口，超时后由 onCreated 兜底
    //
    // 关键点：不使用固定的"猜测"超时，而是利用 onChanged 获取完整元数据后再判断。
    // 这里只是一个启动延迟，等 onDeterminingFilename 有机会先处理。
    // 如果 onDeterminingFilename 已处理，兜底逻辑会跳过。
    //
    // 注意：我们注册一个 onChanged 监听器，一旦 downloadItem 的 filename 或 mime
    // 字段被填充（说明浏览器已解析完响应头），就可以做出更准确的判断。
    //
    // R6-8 修复：加 .catch() 防止内部未捕获异常成为 unhandled rejection 噪音
    startFallbackInterception(downloadId, downloadItem).catch((e) => {
      console.error('[FluxDown] Unexpected error in startFallbackInterception:', e);
    });

    // 30 秒后全面清理
    setTimeout(() => {
      downloadItemCache.delete(downloadId);
      handledDownloads.delete(downloadId);
    }, 30_000);
  });

  /**
   * 兜底拦截入口。
   *
   * 策略：
   * 1. 给第二层（onDeterminingFilename）一个短窗口优先处理（Chrome）。
   * 2. 并发等待两种元数据来源（responseDownloadCache / downloadItem），
   *    共享 META_MAX_WAIT(5s) 总上限，哪个先到走哪条路径。
   * 3. 拿到元数据后做 shouldIntercept 判断，命中则 cancel+erase+发送。
   */
  async function startFallbackInterception(downloadId: number, originalItem: chrome.downloads.DownloadItem) {
    const url = originalItem.url;

    console.log('[FluxDown] startFallbackInterception:', {
      id: downloadId, url,
      cacheHit: responseDownloadCache.has(url),
    });

    // 给第二层一个处理窗口（Chrome 有 onDeterminingFilename，suggest cancel 更干净）
    // Firefox 无此 API，10ms 仅用于让事件循环稳定
    await sleep(hasDeterminingFilename ? 150 : 10);
    if (handledDownloads.has(downloadId)) return;

    // 快速路径：缓存已就绪，直接处理，不进入轮询
    const immediateCached = responseDownloadCache.get(url);
    if (immediateCached) {
      await handleResponseCacheHit(downloadId, url, originalItem, immediateCached);
      return;
    }

    // 慢速路径：两路并发轮询，共享 deadline（Bug 1+6 修复）
    const deadline = Date.now() + META_MAX_WAIT;
    const metaResult = await waitForMeta(url, downloadId, originalItem, deadline);
    if (!metaResult) return; // 被其他层处理或 deadline 到达且结果为 null

    if (handledDownloads.has(downloadId)) return;

    if (metaResult.source === 'responseCache') {
      await handleResponseCacheHit(downloadId, url, originalItem, metaResult.data);
    } else {
      await handleDownloadMetaHit(downloadId, url, originalItem, metaResult.data);
    }
  }

  /** 路径A：基于 responseDownloadCache 的响应头数据做拦截判断 */
  async function handleResponseCacheHit(
    downloadId: number,
    url: string,
    originalItem: chrome.downloads.DownloadItem,
    rc: ResponseDownloadInfo,
  ) {
    if (handledDownloads.has(downloadId)) return;
    console.log('[FluxDown] Fallback path A (response cache):', {
      id: downloadId, url, contentType: rc.contentType,
      contentLength: rc.contentLength, dispositionFilename: rc.dispositionFilename,
    });

    const settings = await getCachedSettings();
    if (!settings.enabled) return;
    if (handledDownloads.has(downloadId)) return;

    const bypass = bypassTokens.get(url);
    if (bypass && bypass > Date.now()) { bypassTokens.delete(url); return; }

    const itemInfo: DownloadItemInfo = {
      url,
      fileSize: rc.contentLength > 0 ? rc.contentLength : undefined,
      mime: rc.contentType || undefined,
      filename: rc.dispositionFilename || originalItem.filename || undefined,
    };

    const intercept = shouldIntercept(itemInfo, settings);
    console.log('[FluxDown] Path A shouldIntercept:', intercept, itemInfo);
    if (!intercept) return;
    if (handledDownloads.has(downloadId)) return;

    // Bug R2-5 修复：用 try/finally 确保无论 executeFallbackIntercept 是否抛出，
    // responseDownloadCache 中对应 URL 的条目都会被清理，防止再次下载同 URL 命中旧缓存
    try {
      await executeFallbackIntercept(downloadId, url, originalItem.referrer, itemInfo);
    } finally {
      responseDownloadCache.delete(url);
    }
  }

  /** 路径B：基于 downloadItem 元数据做拦截判断 */
  async function handleDownloadMetaHit(
    downloadId: number,
    url: string,
    originalItem: chrome.downloads.DownloadItem,
    freshItem: chrome.downloads.DownloadItem,
  ) {
    if (handledDownloads.has(downloadId)) return;
    console.log('[FluxDown] Fallback path B (download meta):', {
      id: downloadId, state: freshItem.state,
      mime: freshItem.mime, filename: freshItem.filename, fileSize: freshItem.fileSize,
    });

    // 下载已完成或被中断，无需处理
    if (freshItem.state === 'complete' || (freshItem as any).state === 'interrupted') {
      console.log('[FluxDown] Path B: download already complete/interrupted, skip');
      return;
    }

    const settings = await getCachedSettings();
    if (!settings.enabled) return;
    if (handledDownloads.has(downloadId)) return;

    const mime = freshItem.mime || originalItem.mime || undefined;
    const fileSize = (freshItem.fileSize > 0 ? freshItem.fileSize : undefined)
      ?? (originalItem.fileSize > 0 ? originalItem.fileSize : undefined);
    const filename = freshItem.filename || originalItem.filename || undefined;

    const itemInfo: DownloadItemInfo = {
      url: freshItem.url || url,
      fileSize,
      mime,
      filename,
    };

    const bypass = bypassTokens.get(url);
    if (bypass && bypass > Date.now()) { bypassTokens.delete(url); return; }

    const intercept = shouldIntercept(itemInfo, settings);
    console.log('[FluxDown] Path B shouldIntercept:', intercept, itemInfo);
    if (!intercept) return;
    if (handledDownloads.has(downloadId)) return;

    await executeFallbackIntercept(downloadId, itemInfo.url, freshItem.referrer || originalItem.referrer, itemInfo);
  }

  /**
   * 执行兜底拦截：cancel + erase + 发送到 FluxDown
   */
  async function executeFallbackIntercept(
    downloadId: number,
    url: string,
    referrer: string | undefined,
    itemInfo: DownloadItemInfo,
  ) {
    // 标记为 fallback 已处理
    handledDownloads.set(downloadId, 'fallback');

    // cancel + erase 并发执行（替代 suggest({ cancel: true })）
    // 必须 await erase，否则下载项残留浏览器下载列表，可能引发二次触发
    await Promise.allSettled([
      browser.downloads.cancel(downloadId).catch((e) => {
        console.warn('[FluxDown] Fallback: failed to cancel download:', e);
      }),
      browser.downloads.erase({ id: downloadId }).catch((e) => {
        console.warn('[FluxDown] Fallback: failed to erase download:', e);
      }),
    ]);

    // 发送到 FluxDown
    // R6-7 修复：sendToFluxDown 抛出时需捕获，否则 startFallbackInterception 成为
    // unhandled rejection（onCreated 以 fire-and-forget 方式调用，无外层 catch）。
    // handledDownloads 已标记为 'fallback'，即使发送失败也不会重复拦截。
    const cleanFilename = extractCleanFilename(itemInfo.filename, url);
    try {
      await sendToFluxDown(url, referrer, cleanFilename, itemInfo.fileSize, itemInfo.mime);
    } catch (e) {
      console.error('[FluxDown] executeFallbackIntercept: sendToFluxDown failed:', e);
    }
  }

  // === 第二层：onDeterminingFilename（主拦截） ===
  // 在浏览器弹出「另存为」对话框之前触发，
  // suggest({ cancel: true }) 可以在不弹出任何浏览器下载 UI 的情况下直接取消下载。
  // Firefox 不支持此 API，完全依赖第三层兜底拦截
  if (browser.downloads.onDeterminingFilename) browser.downloads.onDeterminingFilename.addListener(
    (downloadItem, suggest) => {
      const url = downloadItem.url;

      // 跳过 blob 和 data URL（filename 为空时传 undefined，避免 Chrome 抛出非空校验错误）
      if (url.startsWith('blob:') || url.startsWith('data:')) {
        suggest(downloadItem.filename ? { filename: downloadItem.filename } : (undefined as any));
        return;
      }

      // 如果已被兜底层处理，直接取消（不重复发送）
      if (handledDownloads.get(downloadItem.id) === 'fallback') {
        console.log('[FluxDown] onDeterminingFilename: already handled by fallback, cancelling:', downloadItem.id);
        suggest({ cancel: true });
        return;
      }

      // P0 关键修复：立即预标记为 'primary-pending'，
      // 阻止第三层（onCreated 兜底计时器）在我们异步处理期间竞态抢先执行。
      // 若最终判断不需拦截，在放行时删除此标记。
      handledDownloads.set(downloadItem.id, 'primary');

      // ===== 同步快速路径（修复 Linux 下载栏闪现问题） =====
      // Linux Chrome 在 onCreated 触发时（即 suggest() 异步等待期间）就立即显示下载栏。
      // 若设置缓存已热身，可同步调用 suggest({ cancel: true })，
      // 在 onCreated 触发前完成取消，从而彻底避免下载栏出现。
      // 注：同步调用 suggest 后无需 return true，Chrome 不会再等待异步 suggest。
      const _syncSettings = _settingsCache;
      if (_syncSettings !== null) {
        const _syncBypass = bypassTokens.get(url);
        if (_syncBypass && _syncBypass > Date.now()) {
          bypassTokens.delete(url);
          handledDownloads.delete(downloadItem.id);
          suggest(downloadItem.filename ? { filename: downloadItem.filename } : (undefined as any));
          downloadItemCache.delete(downloadItem.id);
          return;
        }
        if (!_syncSettings.enabled) {
          handledDownloads.delete(downloadItem.id);
          suggest(downloadItem.filename ? { filename: downloadItem.filename } : (undefined as any));
          downloadItemCache.delete(downloadItem.id);
          return;
        }
        const _syncCached = downloadItemCache.get(downloadItem.id);
        const _syncMime = downloadItem.mime || _syncCached?.mime || undefined;
        const _syncFileSize = (downloadItem.fileSize > 0 ? downloadItem.fileSize : undefined)
          ?? (_syncCached && _syncCached.fileSize > 0 ? _syncCached.fileSize : undefined);
        const _syncFilename = downloadItem.filename || _syncCached?.filename || undefined;
        const _syncReferrer = _syncCached?.referrer || undefined;
        const _syncItemInfo: DownloadItemInfo = {
          url, fileSize: _syncFileSize, mime: _syncMime, filename: _syncFilename,
        };
        if (shouldIntercept(_syncItemInfo, _syncSettings)) {
          // 同步取消——在 onCreated 触发前完成，Linux 不会显示下载栏
          suggest({ cancel: true });
          console.log('[FluxDown] Intercepting download (sync-path):', {
            url, mime: _syncMime, filename: _syncFilename,
            fileSize: _syncFileSize, mode: _syncSettings.interceptMode,
          });
          (async () => {
            try {
              try {
                await browser.downloads.erase({ id: downloadItem.id });
              } catch {
                console.debug('[FluxDown] sync-path: erase after cancel (expected)');
              }
              const _syncClean = extractCleanFilename(_syncFilename, url);
              await sendToFluxDown(url, _syncReferrer, _syncClean, _syncFileSize, _syncMime);
            } catch (e) {
              console.error('[FluxDown] sync-path: sendToFluxDown error:', e);
            } finally {
              downloadItemCache.delete(downloadItem.id);
            }
          })();
          return; // 同步 suggest 已调用，无需 return true
        }
        // shouldIntercept=false：若已有足够信息可以确定，同步放行
        if (_syncMime || _syncFilename) {
          handledDownloads.delete(downloadItem.id);
          suggest(downloadItem.filename ? { filename: downloadItem.filename } : (undefined as any));
          downloadItemCache.delete(downloadItem.id);
          return;
        }
        // mime 和 filename 均为空（极少见）→ 降级到下方异步路径
      }

      // 异步判断（缓存未热身 或 metadata 暂缺时的兜底路径）
      (async () => {
        // Bug 2+5 修复：用 suggestCalled 保证 suggest 全局只调用一次。
        // catch 块 + 正常路径都可能调用 suggest，两次调用会导致浏览器行为异常。
        let suggestCalled = false;
        // Bug R4-2 修复：追踪下载是否已被取消（suggest cancel 已调用），
        // 防止 sendToFluxDown 失败时 catch 块误删 handledDownloads 标记导致重复发送。
        let downloadCancelled = false;
        const callSuggest = (arg: chrome.downloads.DownloadFilenameSuggestion | { cancel: true }) => {
          if (suggestCalled) return;
          suggestCalled = true;
          if ('cancel' in arg && arg.cancel) downloadCancelled = true;
          suggest(arg as any);
        };

        try {
          // 再次检查兜底状态（极少数情况：兜底层在预标记之前已完成）
          if (handledDownloads.get(downloadItem.id) === 'fallback') {
            callSuggest({ cancel: true });
            return;
          }

          // P3：使用内存缓存，避免每次拦截都 await storage.sync.get
          const settings = await getCachedSettings();
          if (!settings.enabled) {
            // 不拦截，删除预标记，放行
            handledDownloads.delete(downloadItem.id);
            callSuggest({ filename: downloadItem.filename || undefined } as any);
            return;
          }

          // 检查 Alt+Click 绕过令牌
          const bypass = bypassTokens.get(url);
          if (bypass && bypass > Date.now()) {
            bypassTokens.delete(url);
            // Bug R2-1 修复：删除预标记，让浏览器正常下载
            handledDownloads.delete(downloadItem.id);
            callSuggest({ filename: downloadItem.filename || undefined } as any);
            return;
          }

          // 合并 onCreated 缓存的额外信息
          const cached = downloadItemCache.get(downloadItem.id);
          const mime = downloadItem.mime || cached?.mime || undefined;
          const fileSize = (downloadItem.fileSize > 0 ? downloadItem.fileSize : undefined)
            ?? (cached && cached.fileSize > 0 ? cached.fileSize : undefined);
          const referrer = cached?.referrer || undefined;

          const itemInfo: DownloadItemInfo = {
            url,
            fileSize,
            mime,
            filename: downloadItem.filename || undefined,
          };

          if (!shouldIntercept(itemInfo, settings)) {
            // 不拦截，删除预标记，放行
            handledDownloads.delete(downloadItem.id);
            callSuggest({ filename: downloadItem.filename || undefined } as any);
            return;
          }

          console.log('[FluxDown] Intercepting download (onDeterminingFilename):', {
            url, mime, filename: downloadItem.filename, fileSize, mode: settings.interceptMode,
          });

          // 取消浏览器下载（最干净的方式，在"另存为"弹出前生效）
          callSuggest({ cancel: true });

          // R5-3 修复：suggest({ cancel: true }) 后下载 ID 通常已消失，
          // erase 失败是预期行为（非错误），降级为 debug 日志避免日志噪音。
          try {
            await browser.downloads.erase({ id: downloadItem.id });
          } catch {
            console.debug('[FluxDown] erase after cancel: download item already gone (expected)');
          }

          // 发送到 FluxDown
          const cleanFilename = extractCleanFilename(downloadItem.filename, url);
          await sendToFluxDown(url, referrer, cleanFilename, fileSize, mime);
        } catch (e) {
          console.error('[FluxDown] Error in onDeterminingFilename handler:', e);
          // Bug R4-2 修复：只有在下载尚未被取消（判断阶段出错）时，才清除预标记让兜底层接管。
          // 若下载已被 suggest(cancel) 取消，保留 'primary' 标记，阻止兜底层重复拦截并重复发送。
          if (!downloadCancelled) {
            handledDownloads.delete(downloadItem.id);
            callSuggest({ filename: downloadItem.filename || undefined } as any);
          }
        } finally {
          downloadItemCache.delete(downloadItem.id);
        }
      })();

      // 返回 true 表示 suggest 将被异步调用
      return true;
    },
  );

  // ===== 消息处理（Popup + Content Script） =====
  //
  // 直接返回 Promise，而非 sendResponse + return true。
  //
  // 原因：Firefox MV2 中 "return true + 异步 sendResponse" 模式不可靠——
  // sendResponse 被调用后响应值经常被丢弃，popup 收到 undefined。
  // 返回 Promise 是 Firefox 原生支持的正确方式，Chrome 99+（含 MV3）同样支持。
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  browser.runtime.onMessage.addListener((message, sender, _sendResponse) => {
    return handleMessage(message, sender).catch((_err) => ({ error: String(_err) })) as any;
  });

  // ──────────────────────────────────────────────────────────────
  // 弱网可靠性：发送失败重队列
  // ──────────────────────────────────────────────────────────────
  // 当 App 未运行或网络瞬断导致发送失败时，将请求入队。
  // Service Worker 保活期间持续重试；下次 background 激活时也会恢复。
  // 队列持久化到 chrome.storage.local，防止 SW 被回收时数据丢失。
  interface PendingRequest {
    request: DownloadRequest;
    failedAt: number;
    retries: number;
  }

  const PENDING_QUEUE_KEY = 'pendingDownloadQueue';
  const MAX_PENDING_RETRIES = 5;
  // 指数退避：2^retry * 1000ms，上限 30s
  function retryDelay(retries: number): number {
    return Math.min(Math.pow(2, retries) * 1000, 30_000);
  }

  // Bug 3 修复：用串行 Promise 链保证 enqueuePending / flushPendingQueue
  // 的 read-modify-write 是原子的，杜绝并发竞态导致队列数据丢失
  let _queueChain: Promise<void> = Promise.resolve();

  function enqueuePending(request: DownloadRequest): Promise<void> {
    _queueChain = _queueChain.then(async () => {
      try {
        const stored = await browser.storage.local.get(PENDING_QUEUE_KEY);
        const queue: PendingRequest[] = stored?.[PENDING_QUEUE_KEY] ?? [];
        // Bug 7 修复：去重以 url+filename 联合键，允许同 URL 不同文件名重试
        // 避免用户两次手动发送同一 URL 但第二次被错误去重
        const key = `${request.url}|${request.filename ?? ''}`;
        const isDup = queue.some(
          (p) => `${p.request.url}|${p.request.filename ?? ''}` === key,
        );
        if (!isDup) {
          queue.push({ request, failedAt: Date.now(), retries: 0 });
          await browser.storage.local.set({ [PENDING_QUEUE_KEY]: queue });
          console.log('[FluxDown] Enqueued pending request:', request.url);
        }
      } catch (e) {
        console.warn('[FluxDown] Failed to enqueue pending request:', e);
      }
    });
    return _queueChain;
  }

  function flushPendingQueue(): Promise<void> {
    _queueChain = _queueChain.then(async () => {
      let stored: Record<string, any>;
      try {
        stored = await browser.storage.local.get(PENDING_QUEUE_KEY);
      } catch {
        return;
      }
      const queue: PendingRequest[] = stored[PENDING_QUEUE_KEY] ?? [];
      if (queue.length === 0) return;

      // Bug R2-4 修复：将队列分成"本次需重试"和"暂不需要重试"两组，
      // 对"本次需重试"的条目并发执行（避免串行等待 5 轮 x 10s 的超时），
      // 完成后合并结果，一次写回 storage。
      const now = Date.now();
      const toRetry: PendingRequest[] = [];
      const notYet: PendingRequest[] = [];

      for (const item of queue) {
        if (item.retries >= MAX_PENDING_RETRIES) {
          console.warn('[FluxDown] Pending request exceeded max retries, dropping:', item.request.url);
          continue; // 丢弃
        }
        if (now - item.failedAt < retryDelay(item.retries)) {
          notYet.push(item); // 还没到重试时间
          continue;
        }
        toRetry.push(item);
      }

      if (toRetry.length === 0) {
        // 无需写回（队列内容未变）
        return;
      }

      // 并发重试所有到期条目
      const retryResults = await Promise.allSettled(
        toRetry.map((item) => sendDownloadRequest(item.request)),
      );

      const remaining: PendingRequest[] = [...notYet];
      for (let i = 0; i < toRetry.length; i++) {
        const result = retryResults[i];
        const item = toRetry[i];
        if (result.status === 'fulfilled' && result.value.success) {
          console.log('[FluxDown] Pending request flushed:', item.request.url);
          incrementStat('sent').catch(() => {});
          // 成功则不再放回 remaining
        } else {
          remaining.push({ ...item, retries: item.retries + 1, failedAt: Date.now() });
        }
      }

      try {
        await browser.storage.local.set({ [PENDING_QUEUE_KEY]: remaining });
      } catch { /* ignore */ }
    });
    return _queueChain;
  }

  // 启动时尝试刷新上次未发送的队列
  flushPendingQueue().catch(() => {});
  // 每 30 秒轮询一次
  setInterval(() => { flushPendingQueue().catch(() => {}); }, 30_000);

  // ===== 核心：发送下载请求到 FluxDown App =====
  async function sendToFluxDown(
    url: string,
    referrer?: string,
    filename?: string,
    fileSize?: number,
    mimeType?: string,
  ) {
    // === 提取认证信息（Cookie / Authorization 等） ===
    // 策略 1：从 webRequest 缓存获取（最可靠 — 浏览器真正发出的请求头）
    let cookieString = '';
    const cached = requestHeaderCache.get(url);
    if (cached) {
      cookieString = cached.cookies;
      console.log('[FluxDown] Cookies from webRequest cache:', cookieString.length, 'chars');
      requestHeaderCache.delete(url); // 使用后清理
    }

    // 策略 2：通过 chrome.cookies API 提取（兜底）
    // 加超时保护：弱网下 cookies API 偶发阻塞，500ms 内未返回则跳过
    if (!cookieString) {
      try {
        const cookiesResult = await Promise.race([
          browser.cookies.getAll({ url }),
          new Promise<chrome.cookies.Cookie[]>((_, reject) =>
            setTimeout(() => reject(new Error('cookies timeout')), 500),
          ),
        ]);
        cookieString = cookiesResult.map((c) => `${c.name}=${c.value}`).join('; ');
        console.log('[FluxDown] Cookies from cookies API:', cookiesResult.length, 'cookies');
      } catch (e) {
        console.warn('[FluxDown] Failed/timeout to extract cookies via API:', e);
      }
    }

    const request: DownloadRequest = {
      url,
      filename: filename || '',
      referrer: referrer || '',
      cookies: cookieString,
      fileSize,
      mimeType,
    };

    console.log('[FluxDown] Sending to FluxDown app:', request);

    const response = await sendDownloadRequest(request);

    if (response.success) {
      await incrementStat('sent');
    } else {
      await incrementStat('failed');
      // 发送失败入队，等待 App 启动后重试（弱网 / App 未运行场景）
      await enqueuePending(request);
      notify(
        t('notify.sendFailed'),
        t('notify.connectionFailed', { message: response.message }),
      );
    }
  }

  // ===== 统一消息处理（Popup + Content Script） =====
  async function handleMessage(message: any, sender: chrome.runtime.MessageSender): Promise<any> {
    switch (message.action) {
      // --- Popup 消息 ---
      case 'getStatus': {
        const available = await checkFluxDownAvailable();
        const settings = await loadSettings();
        return { connected: available, settings };
      }

      case 'toggleEnabled': {
        const currentSettings = await loadSettings();
        const newEnabled = !currentSettings.enabled;
        await browser.storage.sync.set({
          settings: { ...currentSettings, enabled: newEnabled },
        });
        updateIcon(newEnabled);
        return { enabled: newEnabled };
      }

      case 'updateSettings': {
        const currentSettings = await loadSettings();
        const merged = { ...currentSettings, ...message.settings };
        await browser.storage.sync.set({ settings: merged });
        return { success: true, settings: merged };
      }

      case 'checkConnection': {
        const isAvailable = await checkFluxDownAvailable();
        return { connected: isAvailable };
      }

      // --- Alt+Click 绕过令牌写入 ---
      case 'addBypassToken': {
        const bypassUrl = message.url as string;
        if (bypassUrl) {
          bypassTokens.set(bypassUrl, Date.now() + 15_000);
        }
        return { success: true };
      }

      // --- Content Script: 资源检测上报 ---
      case 'resourceDetected': {
        const tabId = sender.tab?.id;
        if (!tabId || tabId < 0) return { success: false };

        const pageUrl = sender.tab?.url || sender.url || '';
        const payloads: ResourceMessagePayload[] = message.resources || [];

        if (payloads.length === 0) return { success: true, added: 0 };

        const added = addResources(tabId, pageUrl, payloads);
        if (added > 0) {
          await updateBadgeForTab(tabId);
          await notifyContentScript(tabId);
        }
        return { success: true, added };
      }

      // --- Content Script UI: 请求当前 tab 的资源列表 ---
      case 'getResources': {
        const tabId = sender.tab?.id;
        if (!tabId || tabId < 0) return { resources: [] };
        return { resources: getResourcesForTab(tabId) };
      }

      // --- Content Script UI / Popup: 触发单个资源下载 ---
      case 'downloadResource': {
        const url = message.url as string;
        if (!url) return { success: false, message: 'No URL' };
        const dlSettings = await getCachedSettings();
        if (!dlSettings.enabled) return { success: false, message: 'Extension disabled' };
        await sendToFluxDown(
          url,
          message.referrer,
          message.filename,
          message.fileSize,
          message.mimeType,
        );
        return { success: true };
      }

      // --- Content Script UI: 批量下载多个资源 ---
      // 将所有选中资源的 URL 合并为一个请求发送给桌面应用，
      // 由 Flutter 端的快速下载对话框按换行符拆分后批量创建任务。
      // 不应循环发送多次请求，而是一次性添加全部。
      case 'batchDownload': {
        const items = message.items as Array<{
          url: string;
          referrer?: string;
          filename?: string;
          fileSize?: number;
          mimeType?: string;
        }>;
        if (!Array.isArray(items) || items.length === 0) {
          return { success: false, message: 'No items' };
        }

        // 为每个 URL 提取 cookies，构建 BatchDownloadItem 列表
        // Bug 9 修复：cookies API 加 500ms 超时，与 sendToFluxDown 保持一致
        // Bug R4-6 修复：并发提取所有 URL 的 cookies，避免串行 N×500ms 超时
        const batchItems: BatchDownloadItem[] = await Promise.all(
          items.map(async (item) => {
            let cookieString = '';
            const cached = requestHeaderCache.get(item.url);
            if (cached) {
              cookieString = cached.cookies;
              requestHeaderCache.delete(item.url);
            }
            if (!cookieString) {
              try {
                const cookiesResult = await Promise.race([
                  browser.cookies.getAll({ url: item.url }),
                  new Promise<chrome.cookies.Cookie[]>((_, reject) =>
                    setTimeout(() => reject(new Error('cookies timeout')), 500),
                  ),
                ]);
                cookieString = cookiesResult.map((c) => `${c.name}=${c.value}`).join('; ');
              } catch { /* timeout 或权限不足，跳过 */ }
            }
            return {
              url: item.url,
              referrer: item.referrer || '',
              filename: item.filename,
              cookies: cookieString,
              fileSize: item.fileSize,
              mimeType: item.mimeType,
            };
          }),
        );

        // 单次 HTTP POST 发送所有 URL（用换行符连接）
        const response = await sendBatchDownloadRequest(batchItems);
        if (response.success) {
          await incrementStat('sent');
        } else {
          await incrementStat('failed');
        }
        return { success: response.success, sent: items.length };
      }

      // --- Popup: 切换资源面板显示（发消息给当前活跃 tab 的 Content Script） ---
      case 'toggleResourcePanel': {
        try {
          const [activeTab] = await browser.tabs.query({ active: true, currentWindow: true });
          if (activeTab?.id) {
            await browser.tabs.sendMessage(activeTab.id, { action: 'toggleResourcePanel' });
          }
        } catch {
          // tab 可能未注入 content script
        }
        return { success: true };
      }

      default:
        return { error: 'Unknown action' };
    }
  }

  // ===== 工具函数 =====

  function sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  /**
   * 判断 Content-Type 是否为"下载类型"（即浏览器会将导航转为下载的类型）
   */
  function isDownloadContentType(contentType: string): boolean {
    const ct = contentType.toLowerCase();
    const downloadTypes = [
      'application/octet-stream',
      'application/x-download',
      'application/force-download',
      'application/zip',
      'application/x-rar-compressed',
      'application/x-7z-compressed',
      'application/gzip',
      'application/x-tar',
      'application/x-bzip2',
      'application/x-xz',
      'application/x-msdownload',
      'application/x-msi',
      'application/x-apple-diskimage',
      'application/vnd.debian.binary-package',
      'application/x-iso9660-image',
      'application/x-raw-disk-image',
      'application/pdf',
      'application/vnd.android.package-archive',
      'application/x-bittorrent',
    ];
    // 精确匹配 + 前缀匹配
    if (downloadTypes.includes(ct)) return true;
    if (ct.startsWith('video/') || ct.startsWith('audio/')) return true;
    if (ct.startsWith('application/vnd.openxmlformats-officedocument')) return true;
    if (ct.startsWith('application/vnd.ms-')) return true;
    return false;
  }

  /**
   * 从 Content-Disposition 头解析文件名
   *
   * 支持格式：
   * - Content-Disposition: attachment; filename="report.pdf"
   * - Content-Disposition: attachment; filename=report.pdf
   * - Content-Disposition: attachment; filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf
   */
  function parseContentDispositionFilename(disposition: string): string {
    if (!disposition) return '';

    // 优先尝试 filename*（RFC 5987 编码）
    const starMatch = disposition.match(/filename\*\s*=\s*(?:UTF-8|utf-8)'[^']*'(.+?)(?:;|$)/i);
    if (starMatch) {
      try {
        return decodeURIComponent(starMatch[1].trim());
      } catch {
        // fallthrough
      }
    }

    // 再尝试 filename="..."（带引号）
    const quotedMatch = disposition.match(/filename\s*=\s*"(.+?)"/i);
    if (quotedMatch) {
      return quotedMatch[1];
    }

    // 最后尝试 filename=...（无引号）
    const plainMatch = disposition.match(/filename\s*=\s*([^\s;]+)/i);
    if (plainMatch) {
      return plainMatch[1];
    }

    return '';
  }

  /**
   * 从浏览器的 downloadItem.filename（本地保存路径）和 URL 中提取有意义的文件名。
   *
   * 策略：
   * 1. 如果浏览器给出的 filename 有合法扩展名 → 使用它（浏览器已解析了 Content-Disposition）
   * 2. 否则尝试从 URL 路径提取
   * 3. 如果都无法获得有意义的文件名 → 返回空字符串，交给 Rust 引擎通过 HTTP 探测获取
   */
  function extractCleanFilename(browserFilename: string | undefined, url: string): string {
    // 从浏览器的本地路径中提取纯文件名
    if (browserFilename) {
      // downloadItem.filename 是完整路径，如 "C:\Users\xxx\Downloads\report.pdf"
      // 或 "/home/user/Downloads/report.pdf"
      const basename = browserFilename.split(/[/\\]/).pop() || '';
      if (basename && looksLikeRealFilename(basename)) {
        return basename;
      }
    }

    // 从 URL 路径提取
    try {
      const pathname = new URL(url).pathname;
      const segments = pathname.split('/');
      const lastSegment = decodeURIComponent(segments[segments.length - 1] || '');
      if (lastSegment && looksLikeRealFilename(lastSegment)) {
        return lastSegment;
      }
    } catch {
      // ignore
    }

    // 无法确定有意义的文件名，返回空字符串
    // Rust 端会通过 HTTP HEAD/GET 探测 Content-Disposition 获取真实文件名
    return '';
  }

  /**
   * 判断一个文件名是否看起来像真实的文件名（而非 CDN hash / UUID / 无意义路径段）
   *
   * 真实文件名特征：有常见扩展名，如 "report.pdf", "video.mp4"
   * 非真实文件名：纯 hash "a1b2c3d4e5f6", UUID "550e8400-e29b-41d4-a716-446655440000",
   *               无扩展名 "download", 单字母段 "f", 短 ID "j5g6z92sied"
   */
  function looksLikeRealFilename(name: string): boolean {
    // 必须包含扩展名（至少一个点，且点后有 1-10 个字母/数字）
    const extMatch = name.match(/\.([a-zA-Z0-9]{1,10})$/);
    if (!extMatch) return false;

    // 排除看起来像网页路径的扩展名
    const webExts = ['html', 'htm', 'php', 'asp', 'aspx', 'jsp', 'cgi'];
    if (webExts.includes(extMatch[1].toLowerCase())) return false;

    return true;
  }

  function notify(title: string, message: string) {
    // R5-7 修复：Firefox 下 notifications 可能不存在或受权限限制，
    // fire-and-forget 的未捕获 rejection 会产生控制台错误噪音。
    if (!browser.notifications?.create) return;
    try {
      browser.notifications.create({
        type: 'basic',
        iconUrl: '/icon/128.png',
        title: `FluxDown - ${title}`,
        message,
      });
    } catch (e) {
      console.warn('[FluxDown] notify: failed to create notification:', e);
    }
  }

  function updateIcon(enabled: boolean) {
    const suffix = enabled ? '' : '-disabled';
    const iconPath = {
      16: `/icon/16${suffix}.png`,
      32: `/icon/32${suffix}.png`,
      48: `/icon/48${suffix}.png`,
      128: `/icon/128${suffix}.png`,
    };
    browser.action?.setIcon({ path: iconPath })?.catch(() => { /* 权限不足时静默忽略 */ });
  }

  // 启动时更新图标（settings 已在上方 getCachedSettings 预热，此处复用缓存）
  // 注意：updateIcon 已在 getCachedSettings 预热回调中调用，此行保留为显式确保
});
