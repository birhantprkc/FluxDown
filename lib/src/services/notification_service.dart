import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_local_notifications/flutter_local_notifications.dart';
import 'package:shadcn_ui/shadcn_ui.dart';
import 'package:window_manager/window_manager.dart';

import '../i18n/locale_provider.dart';
import '../models/download_task.dart';
import '../models/settings_provider.dart';
import '../theme/app_colors.dart';
import 'open_folder.dart';
import 'log_service.dart';
import 'win32_toast/win32_toast_window.dart';
import 'windows_toast_helper.dart';

const _tag = 'NotifySvc';

/// 下载完成通知服务 — 通过 OverlayEntry 在主窗口右下角弹出通知卡片。
///
/// ## 设计决策
///
/// 使用主窗口内 OverlayEntry 替代 desktop_multi_window 子窗口方案，
/// 彻底消除子窗口 Isolate 生命周期竞态导致的 0xc0000005 崩溃。
///
/// ## 队列 + 防抖 + 单通知守卫
///
/// 1. 完成通知入队，启动 800ms 防抖定时器
/// 2. 防抖窗口内的多个完成事件合并为一批
/// 3. 同一时刻最多只有一个通知卡片在显示
/// 4. 当前卡片关闭后才处理队列中的下一批
class NotificationService {
  NotificationService._();
  static final instance = NotificationService._();

  static const _appUserModelId = 'Com.FluxDown.App';
  static const _appGuid = '4b648ba5-0b80-4bdb-b2a0-7f3b68c8e2b1';

  GlobalKey<NavigatorState>? _navigatorKey;
  final FlutterLocalNotificationsPlugin _systemNotifications =
      FlutterLocalNotificationsPlugin();
  bool _systemReady = false;

  // ---------------------------------------------------------------------------
  // 队列 + 防抖 + 单通知守卫
  // ---------------------------------------------------------------------------

  /// 等待通知的任务队列
  final List<DownloadTask> _queue = [];

  /// 防抖定时器 — 收集短时间内密集完成的任务
  Timer? _batchTimer;

  /// 当前正在显示的 OverlayEntry
  OverlayEntry? _currentEntry;

  /// 标记是否正在退出
  bool _shuttingDown = false;

  // ---------------------------------------------------------------------------
  // Lifecycle
  // ---------------------------------------------------------------------------

  /// 初始化通知服务 — 传入导航 Key 以获取 Overlay。
  void init({required GlobalKey<NavigatorState> navigatorKey}) {
    _navigatorKey = navigatorKey;
    logInfo(_tag, 'initialized with navigatorKey');
    _initSystemNotifications();
  }

  /// 设置主题提供者（保留 API 兼容性，OverlayEntry 实现中主题通过 context 获取）
  // ignore: avoid_unused_constructor_parameters
  void setThemeProvider(dynamic provider) {
    // OverlayEntry 运行在主窗口 widget tree 内，
    // 主题直接通过 AppColors.of(context) 获取，无需单独传递。
  }

  /// 是否有待处理的通知
  bool get hasPending => _queue.isNotEmpty || _currentEntry != null;

  /// 等待当前通知完成（用于退出前）。
  /// OverlayEntry 不涉及 Isolate，退出时直接移除即可。
  Future<void> waitForPending() async {
    logInfo(_tag, 'waitForPending: hasPending=$hasPending');
    if (!hasPending) return;
    // 给当前通知动画一点时间完成，然后强制清理
    await Future.delayed(const Duration(milliseconds: 500));
    _dismissCurrent();
  }

  /// 标记正在退出，停止接受新通知
  void shutdown() {
    logInfo(_tag, 'shutdown called');
    _shuttingDown = true;
    _batchTimer?.cancel();
    _dismissCurrent();
    if (Platform.isWindows) {
      Win32ToastWindow.instance.destroyAll();
    }
  }

  // ---------------------------------------------------------------------------
  // Public API
  // ---------------------------------------------------------------------------

  /// 显示下载完成的通知。
  ///
  /// 所有平台统一走 800ms 防抖队列，合并短时间内密集完成的任务后一次性派发：
  /// - 窗口前台：OverlayEntry 精美卡片
  /// - 窗口不可见/最小化（Windows）：Win32 悬浮窗（绕过通知设置，100% 可靠）
  void showDownloadComplete(DownloadTask task) {
    if (SettingsProvider.globalInstance?.notifyOnComplete == false) {
      logInfo(_tag, 'showDownloadComplete: skipped (notifyOnComplete=false)');
      return;
    }
    _showSystemDownloadComplete(task);
    logInfo(
      _tag,
      'showDownloadComplete: file=${task.fileName}, shuttingDown=$_shuttingDown',
    );
    if (_shuttingDown) {
      logInfo(_tag, 'skipped (shuttingDown)');
      return;
    }
    _enqueueTask(task);
  }

  /// 入队 + 启动防抖定时器（所有平台统一路径）。
  void _enqueueTask(DownloadTask task) {
    _queue.add(task);
    logInfo(_tag, 'queued, queueSize=${_queue.length}');

    // 如果 OverlayEntry 正在显示，仅入队等其关闭后再处理
    if (_currentEntry != null) {
      logInfo(_tag, 'notification active, queued for later');
      return;
    }

    // 防抖：等待 800ms 收集短时间内密集完成的任务
    _batchTimer?.cancel();
    _batchTimer = Timer(const Duration(milliseconds: 800), _flushQueue);
  }

  /// 多个任务全部完成后的系统通知（汇总）
  void showAllCompletedSummary(int count) {
    if (count < 2) return;
    if (SettingsProvider.globalInstance?.notifyOnComplete == false) {
      logInfo(
        _tag,
        'showAllCompletedSummary: skipped (notifyOnComplete=false)',
      );
      return;
    }
    _showSystemAllCompleted(count);
  }

  // ---------------------------------------------------------------------------
  // Internal
  // ---------------------------------------------------------------------------

  /// 冲刷队列 — 取出所有待处理任务，整批派发给合适的通知路径。
  void _flushQueue() {
    if (_currentEntry != null || _queue.isEmpty || _shuttingDown) return;

    final batch = List<DownloadTask>.of(_queue);
    _queue.clear();
    logInfo(_tag, 'flushing ${batch.length} notifications');

    if (Platform.isWindows) {
      _flushQueueWindows(batch);
    } else if (Platform.isMacOS || Platform.isLinux) {
      _flushQueueDesktop(batch);
    } else {
      _showOverlay(batch);
    }
  }

  /// Windows：做一次窗口可见性检查，整批派发（前台→OverlayEntry，后台→Win32Toast）。
  Future<void> _flushQueueWindows(List<DownloadTask> batch) async {
    try {
      final isVisible = await windowManager.isVisible();
      final isMinimized = await windowManager.isMinimized();
      if (_shuttingDown) return;

      if (isVisible && !isMinimized) {
        logInfo(_tag, 'window visible, using overlay (batch=${batch.length})');
        _showOverlay(batch);
      } else {
        logInfo(
          _tag,
          'window hidden/minimized, using Win32 toast (batch=${batch.length})',
        );
        final brightness =
            WidgetsBinding.instance.platformDispatcher.platformBrightness;
        Win32ToastWindow.instance.isDarkMode = brightness == Brightness.dark;
        Win32ToastWindow.instance.enqueueBatch(batch);
      }
    } catch (e, stack) {
      logError(_tag, 'failed to check window visibility', e, stack);
      _showOverlay(batch);
    }
  }

  /// macOS / Linux：窗口可见时用 OverlayEntry，后台/最小化时用系统通知。
  Future<void> _flushQueueDesktop(List<DownloadTask> batch) async {
    try {
      final isVisible = await windowManager.isVisible();
      final isMinimized = await windowManager.isMinimized();
      if (_shuttingDown) return;

      if (isVisible && !isMinimized) {
        logInfo(
          _tag,
          'desktop: window visible, using overlay (batch=${batch.length})',
        );
        _showOverlay(batch);
      } else {
        logInfo(
          _tag,
          'desktop: window hidden/minimized, using system notification '
          '(batch=${batch.length})',
        );
        for (final task in batch) {
          await _showSystemDownloadComplete(task);
        }
      }
    } catch (e, stack) {
      logError(_tag, 'desktop: failed to check window visibility', e, stack);
      _showOverlay(batch);
    }
  }

  void _showOverlay(List<DownloadTask> batch) {
    final overlay = _navigatorKey?.currentState?.overlay;
    if (overlay == null) {
      logInfo(_tag, 'no overlay available, skipping notification');
      return;
    }

    _currentEntry = OverlayEntry(
      builder: (context) => _NotificationCard(
        task: batch.last,
        taskCount: batch.length,
        onDismiss: _onNotificationDismissed,
      ),
    );
    overlay.insert(_currentEntry!);
    logInfo(_tag, 'overlay notification shown');
  }

  void _onNotificationDismissed() {
    _dismissCurrent();

    // 处理守卫期间新入队的任务
    if (_queue.isNotEmpty && !_shuttingDown) {
      // 短暂延迟，避免视觉上的连续闪烁
      Future.delayed(const Duration(milliseconds: 300), _flushQueue);
    }
  }

  void _dismissCurrent() {
    _currentEntry?.remove();
    _currentEntry = null;
  }

  Future<void> _initSystemNotifications() async {
    if (_systemReady) {
      logInfo(_tag, 'initSystem: already ready, skipping');
      return;
    }
    logInfo(_tag, 'initSystem: starting initialization...');

    // Windows 10: ensure Start Menu shortcut with AUMID exists.
    // Without this, Toast API returns success but nothing is displayed.
    if (Platform.isWindows) {
      await ensureWindowsToastShortcut(
        appName: 'FluxDown',
        aumid: _appUserModelId,
        clsid: _appGuid,
      );
    }

    try {
      const windows = WindowsInitializationSettings(
        appName: 'FluxDown',
        appUserModelId: _appUserModelId,
        guid: _appGuid,
      );
      const linux = LinuxInitializationSettings(defaultActionName: 'open');
      const darwin = DarwinInitializationSettings(
        requestAlertPermission: false,
        requestBadgePermission: false,
        requestSoundPermission: false,
      );
      const settings = InitializationSettings(
        windows: windows,
        linux: linux,
        macOS: darwin,
      );

      logInfo(
        _tag,
        'initSystem: calling initialize(aumid=$_appUserModelId, '
        'guid=$_appGuid)',
      );
      final result = await _systemNotifications.initialize(
        settings: settings,
        onDidReceiveNotificationResponse: _onSystemNotificationResponse,
      );
      logInfo(_tag, 'initSystem: initialize result=$result');

      // macOS: explicitly request notification permissions after initialize.
      // Without this step the system never prompts the user and all show()
      // calls silently fail.
      if (Platform.isMacOS) {
        final macOsPlugin = _systemNotifications
            .resolvePlatformSpecificImplementation<
              IOSFlutterLocalNotificationsPlugin
            >();
        // On macOS the plugin exposes the same API via the macOS implementation.
        final macPlugin = _systemNotifications
            .resolvePlatformSpecificImplementation<
              MacOSFlutterLocalNotificationsPlugin
            >();
        if (macPlugin != null) {
          final granted = await macPlugin.requestPermissions(
            alert: true,
            badge: true,
            sound: true,
          );
          logInfo(_tag, 'initSystem: macOS permission granted=$granted');
        } else {
          logInfo(_tag, 'initSystem: macOS plugin not resolved, skipping permission request');
          // Fallback: try iOS plugin (older plugin versions share the same class)
          if (macOsPlugin != null) {
            final granted = await macOsPlugin.requestPermissions(
              alert: true,
              badge: true,
              sound: true,
            );
            logInfo(_tag, 'initSystem: macOS(iOS-compat) permission granted=$granted');
          }
        }
      }

      _systemReady = true;
      logInfo(_tag, 'initSystem: success');
    } catch (e, stack) {
      logError(_tag, 'initSystem: FAILED', e, stack);
    }
  }

  Future<void> _showSystemDownloadComplete(DownloadTask task) async {
    logInfo(
      _tag,
      'systemNotify: start, file=${task.fileName}, '
      'shuttingDown=$_shuttingDown, systemReady=$_systemReady',
    );
    if (_shuttingDown) return;
    await _initSystemNotifications();
    if (!_systemReady) {
      logInfo(_tag, 'systemNotify: skipped — system not ready');
      return;
    }

    try {
      final s = currentS;
      final payload = _buildPayload(
        action: 'open_file',
        filePath: _resolveFilePath(task),
      );

      final details = NotificationDetails(
        windows: WindowsNotificationDetails(
          actions: [
            WindowsAction(content: s.openFileFolder, arguments: 'open_folder'),
            WindowsAction(content: s.openFile, arguments: 'open_file'),
          ],
        ),
        linux: const LinuxNotificationDetails(defaultActionName: 'open'),
        macOS: const DarwinNotificationDetails(),
      );

      final notifId = task.id.hashCode;
      logInfo(
        _tag,
        'systemNotify: calling show(id=$notifId, '
        'title="${s.downloadCompleted}", body="${task.fileName}")',
      );

      await _systemNotifications.show(
        id: notifId,
        title: s.downloadCompleted,
        body: task.fileName,
        notificationDetails: details,
        payload: payload,
      );
      logInfo(_tag, 'systemNotify: show() completed successfully');
    } catch (e, stack) {
      logError(_tag, 'systemNotify: show() failed', e, stack);
    }
  }

  Future<void> _showSystemAllCompleted(int count) async {
    logInfo(
      _tag,
      'systemBatchNotify: start, count=$count, '
      'shuttingDown=$_shuttingDown, systemReady=$_systemReady',
    );
    if (_shuttingDown) return;
    await _initSystemNotifications();
    if (!_systemReady) {
      logInfo(_tag, 'systemBatchNotify: skipped — system not ready');
      return;
    }

    try {
      final s = currentS;
      final title = s.batchDownloadCompleted(count);
      final details = NotificationDetails(
        windows: const WindowsNotificationDetails(),
        linux: const LinuxNotificationDetails(defaultActionName: 'open'),
        macOS: const DarwinNotificationDetails(),
      );

      final notifId = DateTime.now().millisecondsSinceEpoch ~/ 1000;
      logInfo(
        _tag,
        'systemBatchNotify: calling show(id=$notifId, '
        'title="$title")',
      );

      await _systemNotifications.show(
        id: notifId,
        title: title,
        body: s.downloadCompleted,
        notificationDetails: details,
        payload: _buildPayload(
          action: 'open_folder',
          filePath: _resolveDefaultDir(),
        ),
      );
      logInfo(_tag, 'systemBatchNotify: show() completed successfully');
    } catch (e, stack) {
      logError(_tag, 'systemBatchNotify: show() failed', e, stack);
    }
  }

  String _buildPayload({required String action, required String filePath}) {
    return '$action|$filePath';
  }

  String _resolveFilePath(DownloadTask task) {
    return '${task.saveDir}${Platform.pathSeparator}${task.fileName}';
  }

  void _onSystemNotificationResponse(NotificationResponse response) {
    final payload = response.payload ?? '';
    final actionId = response.actionId ?? '';
    final parts = payload.split('|');
    final action = actionId.isNotEmpty
        ? actionId
        : (parts.isNotEmpty ? parts[0] : '');
    final filePath = parts.length > 1 ? parts[1] : '';
    if (action == 'open_folder') {
      _openFolder(filePath);
      return;
    }
    if (action == 'open_file') {
      _openFile(filePath);
      return;
    }

    // Default: open folder for safety
    _openFolder(filePath);
  }

  Future<void> _openFile(String filePath) async {
    if (filePath.isEmpty) return;
    await openFile(filePath);
  }

  Future<void> _openFolder(String filePath) async {
    final resolved = filePath.isEmpty ? _resolveDefaultDir() : filePath;
    if (resolved.isEmpty) return;
    await openFolder(resolved);
  }

  String _resolveDefaultDir() {
    final home =
        Platform.environment['USERPROFILE'] ??
        Platform.environment['HOME'] ??
        '.';
    return '$home${Platform.pathSeparator}Downloads';
  }
}

// =============================================================================
// 通知卡片组件 — 在主窗口右下角显示
// =============================================================================

class _NotificationCard extends StatefulWidget {
  final DownloadTask task;
  final int taskCount;
  final VoidCallback onDismiss;

  const _NotificationCard({
    required this.task,
    required this.taskCount,
    required this.onDismiss,
  });

  @override
  State<_NotificationCard> createState() => _NotificationCardState();
}

class _NotificationCardState extends State<_NotificationCard>
    with SingleTickerProviderStateMixin {
  bool _isHovered = false;
  bool _closed = false;
  late final AnimationController _progressController;

  // 入场/退场动画值
  double _slideOffset = 1.0; // 1.0 = 完全在屏幕外, 0.0 = 可见
  double _opacity = 0.0;

  String get fileName => widget.task.fileName;
  String get fileSize => widget.task.sizeText;
  String get fileExt => widget.task.fileExtension;
  String get filePath =>
      '${widget.task.saveDir}${Platform.pathSeparator}${widget.task.fileName}';
  int get taskCount => widget.taskCount;
  bool get isBatch => taskCount > 1;

  static const _cardWidth = 340.0;
  static const _cardHeight = 158.0;
  static const _autoCloseDuration = Duration(seconds: 8);
  static const _slideInDuration = Duration(milliseconds: 300);
  static const _slideOutDuration = Duration(milliseconds: 200);

  @override
  void initState() {
    super.initState();
    _progressController = AnimationController(
      vsync: this,
      duration: _autoCloseDuration,
    )..addStatusListener(_onAnimationStatus);

    // 入场动画
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _animateIn();
    });
  }

  @override
  void dispose() {
    _progressController.dispose();
    super.dispose();
  }

  Future<void> _animateIn() async {
    final start = DateTime.now();
    while (true) {
      final elapsed = DateTime.now().difference(start);
      final t = (elapsed.inMilliseconds / _slideInDuration.inMilliseconds)
          .clamp(0.0, 1.0);
      // ease-out cubic
      final curved = 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t);
      if (mounted) {
        setState(() {
          _slideOffset = 1.0 - curved;
          _opacity = curved;
        });
      }
      if (t >= 1.0) break;
      await Future.delayed(const Duration(milliseconds: 16));
    }
    // 入场动画完成后开始自动关闭倒计时
    _progressController.forward();
  }

  Future<void> _animateOut() async {
    final start = DateTime.now();
    while (true) {
      final elapsed = DateTime.now().difference(start);
      final t = (elapsed.inMilliseconds / _slideOutDuration.inMilliseconds)
          .clamp(0.0, 1.0);
      if (mounted) {
        setState(() {
          _slideOffset = t;
          _opacity = 1.0 - t;
        });
      }
      if (t >= 1.0) break;
      await Future.delayed(const Duration(milliseconds: 16));
    }
    widget.onDismiss();
  }

  void _onAnimationStatus(AnimationStatus status) {
    if (status == AnimationStatus.completed && !_isHovered) {
      _close();
    }
  }

  void _close() {
    if (_closed) return;
    _closed = true;
    _progressController.stop();
    _animateOut();
  }

  Future<void> _openFile() async {
    await openFile(filePath);
    _close();
  }

  Future<void> _openFolder() async {
    await openFolder(filePath);
    _close();
  }

  @override
  Widget build(BuildContext context) {
    final c = AppColors.of(context);
    final s = LocaleScope.of(context);

    return Positioned(
      right: 16,
      bottom: 16,
      child: Transform.translate(
        offset: Offset(_slideOffset * (_cardWidth + 32), 0),
        child: Opacity(
          opacity: _opacity,
          child: Material(
            color: Colors.transparent,
            child: Container(
              width: _cardWidth,
              height: _cardHeight,
              decoration: BoxDecoration(
                color: c.dialogBg,
                borderRadius: BorderRadius.circular(12),
                border: Border.all(color: c.border),
                boxShadow: [
                  BoxShadow(
                    color: Colors.black.withValues(alpha: 0.15),
                    blurRadius: 20,
                    offset: const Offset(0, 8),
                  ),
                  BoxShadow(
                    color: Colors.black.withValues(alpha: 0.05),
                    blurRadius: 6,
                    offset: const Offset(0, 2),
                  ),
                ],
              ),
              child: MouseRegion(
                onEnter: (_) {
                  _isHovered = true;
                  _progressController.stop();
                },
                onExit: (_) {
                  _isHovered = false;
                  if (_progressController.isCompleted) {
                    _close();
                  } else {
                    _progressController.forward();
                  }
                },
                child: Column(
                  children: [
                    // === 顶部自动关闭进度条（2px）===
                    ClipRRect(
                      borderRadius: const BorderRadius.vertical(
                        top: Radius.circular(12),
                      ),
                      child: AnimatedBuilder(
                        animation: _progressController,
                        builder: (context, _) {
                          return LinearProgressIndicator(
                            value: 1.0 - _progressController.value,
                            minHeight: 2,
                            backgroundColor: Colors.transparent,
                            valueColor: AlwaysStoppedAnimation(
                              c.accent.withValues(alpha: 0.4),
                            ),
                          );
                        },
                      ),
                    ),
                    // === Header ===
                    Padding(
                      padding: const EdgeInsets.fromLTRB(14, 8, 6, 0),
                      child: Row(
                        children: [
                          Container(
                            width: 18,
                            height: 18,
                            decoration: BoxDecoration(
                              color: AppColors.green.withValues(alpha: 0.12),
                              borderRadius: BorderRadius.circular(9),
                            ),
                            child: const Icon(
                              LucideIcons.check,
                              size: 11,
                              color: AppColors.green,
                            ),
                          ),
                          const SizedBox(width: 7),
                          Text(
                            isBatch
                                ? s.batchDownloadCompleted(taskCount)
                                : s.downloadCompleted,
                            style: TextStyle(
                              fontSize: 13,
                              fontWeight: FontWeight.w600,
                              color: c.textPrimary,
                            ),
                          ),
                          const Spacer(),
                          _CloseButton(onTap: _close, colors: c),
                        ],
                      ),
                    ),
                    // === File info ===
                    Expanded(
                      child: Padding(
                        padding: const EdgeInsets.fromLTRB(14, 6, 14, 6),
                        child: Row(
                          children: [
                            Container(
                              width: 38,
                              height: 38,
                              decoration: BoxDecoration(
                                color: c.surface2,
                                borderRadius: BorderRadius.circular(8),
                                border: Border.all(
                                  color: c.border.withValues(alpha: 0.5),
                                ),
                              ),
                              child: Center(
                                child: Text(
                                  fileExt.toLowerCase(),
                                  style: TextStyle(
                                    fontSize: 10,
                                    fontWeight: FontWeight.w600,
                                    color: c.accent,
                                    letterSpacing: 0.3,
                                  ),
                                ),
                              ),
                            ),
                            const SizedBox(width: 10),
                            Expanded(
                              child: Column(
                                mainAxisAlignment: MainAxisAlignment.center,
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    fileName,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: TextStyle(
                                      fontSize: 12.5,
                                      fontWeight: FontWeight.w500,
                                      color: c.textPrimary,
                                    ),
                                  ),
                                  const SizedBox(height: 2),
                                  Text(
                                    isBatch
                                        ? s.andMoreFiles(taskCount - 1)
                                        : fileSize,
                                    style: TextStyle(
                                      fontSize: 11,
                                      color: c.textMuted,
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                    // === Divider ===
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 14),
                      child: Divider(height: 1, color: c.border),
                    ),
                    // === Actions ===
                    Padding(
                      padding: const EdgeInsets.fromLTRB(14, 8, 14, 10),
                      child: Row(
                        children: [
                          Expanded(
                            child: ShadButton.outline(
                              size: ShadButtonSize.sm,
                              onPressed: _openFolder,
                              child: Row(
                                mainAxisAlignment: MainAxisAlignment.center,
                                children: [
                                  Icon(
                                    LucideIcons.folderOpen,
                                    size: 13,
                                    color: c.textSecondary,
                                  ),
                                  const SizedBox(width: 6),
                                  Text(
                                    s.openFileFolder,
                                    style: TextStyle(
                                      fontSize: 12,
                                      color: c.textPrimary,
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          ),
                          const SizedBox(width: 8),
                          Expanded(
                            child: ShadButton(
                              size: ShadButtonSize.sm,
                              onPressed: _openFile,
                              child: Row(
                                mainAxisAlignment: MainAxisAlignment.center,
                                children: [
                                  const Icon(
                                    LucideIcons.externalLink,
                                    size: 13,
                                    color: Colors.white,
                                  ),
                                  const SizedBox(width: 6),
                                  Text(
                                    s.openFile,
                                    style: const TextStyle(
                                      fontSize: 12,
                                      color: Colors.white,
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 精致的关闭按钮
class _CloseButton extends StatefulWidget {
  final VoidCallback onTap;
  final AppColors colors;

  const _CloseButton({required this.onTap, required this.colors});

  @override
  State<_CloseButton> createState() => _CloseButtonState();
}

class _CloseButtonState extends State<_CloseButton> {
  bool _isHovered = false;

  @override
  Widget build(BuildContext context) {
    final c = widget.colors;
    return MouseRegion(
      onEnter: (_) => setState(() => _isHovered = true),
      onExit: (_) => setState(() => _isHovered = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: Container(
          width: 26,
          height: 26,
          decoration: BoxDecoration(
            color: _isHovered ? c.surface3 : Colors.transparent,
            borderRadius: BorderRadius.circular(6),
          ),
          child: Icon(
            LucideIcons.x,
            size: 13,
            color: _isHovered ? c.textPrimary : c.textMuted,
          ),
        ),
      ),
    );
  }
}
