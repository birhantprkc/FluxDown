import 'dart:async';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';
import '../../models/download_task.dart';
import '../log_service.dart';
import '../open_folder.dart';
import 'win32_bindings.dart';

const _tag = 'Win32Toast';

// =============================================================================
// 颜色方案
// =============================================================================

class _ToastColors {
  final int bg;
  final int text;
  final int textMuted;
  final int border;
  final int btnHover;
  final int accent;
  final int green;
  final int divider;

  const _ToastColors({
    required this.bg,
    required this.text,
    required this.textMuted,
    required this.border,
    required this.btnHover,
    required this.accent,
    required this.green,
    required this.divider,
  });

  static const dark = _ToastColors(
    bg: 0x1F1F1F,
    text: 0xF5F5F5,
    textMuted: 0x999999,
    border: 0x3A3A3A,
    btnHover: 0x3D3D3D,
    accent: 0x4A9EFF,
    green: 0x4CAF50,
    divider: 0x2D2D2D,
  );

  static const light = _ToastColors(
    bg: 0xFFFFFF,
    text: 0x1A1A1A,
    textMuted: 0x888888,
    border: 0xE0E0E0,
    btnHover: 0xE8E8E8,
    accent: 0x0066CC,
    green: 0x4CAF50,
    divider: 0xF0F0F0,
  );
}

// =============================================================================
// 生命周期阶段
// =============================================================================

enum _Phase { fadeIn, autoClose, fadeOut }

// =============================================================================
// 窗口状态（纯 Dart，无 Win32 消息参与）
// =============================================================================

class _ToastState {
  final String title;
  final String body;
  final String fileExt;
  final String filePath;
  final int batchCount;
  final void Function() onOpenFile;
  final void Function() onOpenFolder;
  final void Function() onDismissed;

  _Phase phase = _Phase.fadeIn;
  int alpha = 0;
  bool isHovered = false;
  bool prevMouseDown = false; // 上一个 tick 时鼠标是否按下
  int hoveredButton = 0; // 0=无 1=关闭 2=打开文件夹 3=打开文件

  // autoClose 阶段开始时间
  DateTime? autoCloseStart;

  // GDI 字体句柄（窗口销毁时释放）
  int hFontTitle = 0;
  int hFontBody = 0;

  // 物理尺寸（DPI 缩放后）
  int scaledW = 0;
  int scaledH = 0;

  // 命中测试区域（物理像素，客户端坐标）
  int closeX1 = 0, closeY1 = 0, closeX2 = 0, closeY2 = 0;
  int folderX1 = 0, folderY1 = 0, folderX2 = 0, folderY2 = 0;
  int fileX1 = 0, fileY1 = 0, fileX2 = 0, fileY2 = 0;

  _ToastState({
    required this.title,
    required this.body,
    required this.fileExt,
    required this.filePath,
    required this.batchCount,
    required this.onOpenFile,
    required this.onOpenFolder,
    required this.onDismissed,
  });
}

// =============================================================================
// 全局状态
// =============================================================================

final Map<int, _ToastState> _states = {}; // hwnd → state
bool _classRegistered = false;
const String _className = 'FluxDownToast_v2';

// 复用的 POINT 指针：避免每 tick calloc/free（16ms × 整个 Toast 生命周期）
final _sharedCursorPt = calloc<POINT>();

// =============================================================================
// 批次数据
// =============================================================================

/// 一个通知批次 — 包含代表任务（显示用）和批次总数
class _ToastBatch {
  final DownloadTask representative;
  final int count;

  _ToastBatch(this.representative, this.count);
}

// =============================================================================
// Win32ToastWindow — 公开 API
// =============================================================================

/// Win32 悬浮通知窗口。
///
/// ## 设计原则
///
/// 为避免 "Cannot invoke native callback outside an isolate" 崩溃：
/// - WndProc 直接使用 `DefWindowProcW` 原生函数指针（纯 Win32，无 Dart 回调）
/// - 所有状态机逻辑（淡入/倒计时/淡出）由 Dart `Timer.periodic` 驱动
/// - 鼠标输入通过 `GetCursorPos` + `GetAsyncKeyState` 轮询实现
/// - 绘制通过 `GetDC` + GDI 在 Timer 回调中完成（无 WM_PAINT）
class Win32ToastWindow {
  Win32ToastWindow._();
  static final instance = Win32ToastWindow._();

  /// 是否为深色模式
  bool isDarkMode = false;

  final List<_ToastBatch> _queue = [];
  bool _showing = false;
  Timer? _masterTimer;

  /// 将一批下载任务加入显示队列（800ms 防抖后由 NotificationService 调用）。
  ///
  /// 批次中的所有任务对应一个 Toast：
  /// - count=1 → 显示文件名 + "下载完成"
  /// - count>1 → 显示代表文件名 + "N个文件已下载"
  void enqueueBatch(List<DownloadTask> tasks) {
    if (!Platform.isWindows || tasks.isEmpty) return;
    if (_queue.length >= 5) _queue.removeAt(0);
    final batch = _ToastBatch(tasks.last, tasks.length);
    _queue.add(batch);
    logInfo(
      _tag,
      'enqueueBatch: count=${tasks.length}, '
      'representative=${tasks.last.fileName}, queueSize=${_queue.length}',
    );
    _tryShowNext();
  }

  /// 销毁所有 Toast（应用退出时调用）
  void destroyAll() {
    _masterTimer?.cancel();
    _masterTimer = null;
    final hwnds = List<int>.from(_states.keys);
    for (final hwnd in hwnds) {
      _releaseWindowResources(hwnd);
      try {
        destroyWindow(hwnd);
      } catch (e) {
        logError(_tag, 'destroyAll: destroyWindow($hwnd) failed', e);
      }
    }
    _states.clear();
    _queue.clear();
    _showing = false;
    logInfo(_tag, 'destroyAll: done');
  }

  void _tryShowNext() {
    if (_showing || _queue.isEmpty) return;
    final batch = _queue.removeAt(0);
    _showing = true;
    _showBatch(batch);
  }

  void _showBatch(_ToastBatch batch) {
    try {
      _ensureClassRegistered();
      _createToastWindow(batch);
      _ensureMasterTimer();
    } catch (e, stack) {
      logError(_tag, 'showBatch failed', e, stack);
      _showing = false;
      Future.delayed(const Duration(milliseconds: 500), _tryShowNext);
    }
  }

  void _ensureMasterTimer() {
    if (_masterTimer?.isActive == true) return;
    _masterTimer = Timer.periodic(
      const Duration(milliseconds: 16),
      _onMasterTick,
    );
  }

  void _onMasterTick(Timer _) {
    if (_states.isEmpty) {
      _masterTimer?.cancel();
      _masterTimer = null;
      return;
    }
    final entries = Map<int, _ToastState>.from(_states);
    for (final MapEntry(:key, :value) in entries.entries) {
      _processWindowTick(key, value);
    }
  }

  void _onToastDismissed() {
    _showing = false;
    Future.delayed(const Duration(milliseconds: 400), _tryShowNext);
  }
}

// =============================================================================
// 窗口类注册（使用 DefWindowProcW 原生指针，无 Dart 回调）
// =============================================================================

void _ensureClassRegistered() {
  if (_classRegistered) return;

  final hInstance = getModuleHandleW(nullptr);
  final classNamePtr = _className.toNativeUtf16();
  final cursorPtr = Pointer<Utf16>.fromAddress(32512); // IDC_ARROW

  final wndClass = calloc<WNDCLASSEXW>();
  try {
    wndClass.ref.cbSize = sizeOf<WNDCLASSEXW>();
    wndClass.ref.style = 0;
    // 直接使用 DefWindowProcW 的原生函数指针 — 不经过 Dart VM
    wndClass.ref.lpfnWndProc = defWindowProcWPtr;
    wndClass.ref.cbClsExtra = 0;
    wndClass.ref.cbWndExtra = 0;
    wndClass.ref.hInstance = hInstance;
    wndClass.ref.hIcon = 0;
    wndClass.ref.hCursor = loadCursorW(0, cursorPtr);
    wndClass.ref.hbrBackground = 0; // 自绘，不使用系统背景刷
    wndClass.ref.lpszMenuName = nullptr;
    wndClass.ref.lpszClassName = classNamePtr;
    wndClass.ref.hIconSm = 0;

    final atom = registerClassExW(wndClass);
    if (atom == 0) throw StateError('RegisterClassExW failed');
    _classRegistered = true;
    logInfo(_tag, 'window class registered, atom=$atom');
  } finally {
    calloc.free(wndClass);
    calloc.free(classNamePtr);
  }
}

// =============================================================================
// 创建窗口
// =============================================================================

void _createToastWindow(_ToastBatch batch) {
  const logicalW = 340;
  const logicalH = 130;

  final task = batch.representative;

  final workArea = calloc<RECT>();
  try {
    systemParametersInfoW(SPI_GETWORKAREA, 0, workArea.cast(), 0);

    final classNamePtr = _className.toNativeUtf16();
    final titlePtr = ''.toNativeUtf16();

    try {
      final exStyle =
          WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED;

      // 先以逻辑坐标创建，获取 hwnd 后读取 DPI 再调整
      final hwnd = createWindowExW(
        exStyle,
        classNamePtr,
        titlePtr,
        WS_POPUP,
        workArea.ref.right - logicalW - 12,
        workArea.ref.bottom - logicalH - 12,
        logicalW,
        logicalH,
        0,
        0,
        getModuleHandleW(nullptr),
        nullptr,
      );
      if (hwnd == 0) throw StateError('CreateWindowExW returned 0');

      // DPI 感知
      final dpi = getDpiForWindow(hwnd);
      final scale = dpi / 96.0;
      final scaledW = (logicalW * scale).round();
      final scaledH = (logicalH * scale).round();
      final scaledX = workArea.ref.right - scaledW - 12;
      final scaledY = workArea.ref.bottom - scaledH - 12;

      // 构建状态（批量时标题显示数量）
      final filePath =
          '${task.saveDir}${Platform.pathSeparator}${task.fileName}';
      final title =
          batch.count > 1 ? '${batch.count}个文件已下载' : '下载完成';
      final state = _ToastState(
        title: title,
        body: task.fileName,
        fileExt: task.fileExtension.toUpperCase(),
        filePath: filePath,
        batchCount: batch.count,
        onOpenFile: () => openFile(filePath),
        onOpenFolder: () => openFolder(filePath),
        onDismissed: Win32ToastWindow.instance._onToastDismissed,
      );

      state.scaledW = scaledW;
      state.scaledH = scaledH;
      _calcHitAreas(state, scaledW, scaledH, scale);

      state.hFontTitle = _createGdiFont((13 * scale).round(), FW_SEMIBOLD);
      state.hFontBody = _createGdiFont((11 * scale).round(), FW_NORMAL);

      _states[hwnd] = state;

      // 应用圆角
      _applyRoundCorners(hwnd, scaledW, scaledH);

      // 调整到 DPI 校正后的位置和大小
      setWindowPos(
        hwnd,
        HWND_TOPMOST,
        scaledX,
        scaledY,
        scaledW,
        scaledH,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
      );

      // 初始透明度 0（不可见），Timer 驱动淡入
      setLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
      showWindow(hwnd, SW_SHOWNOACTIVATE);

      // 立即绘制内容（alpha=0 时用户不可见，但内容已写入 DC）
      _repaintWindow(hwnd, state);

      logInfo(
        _tag,
        'toast created hwnd=$hwnd, dpi=$dpi, '
        'scale=$scale, size=${scaledW}x$scaledH',
      );
    } finally {
      calloc.free(classNamePtr);
      calloc.free(titlePtr);
    }
  } finally {
    calloc.free(workArea);
  }
}

void _calcHitAreas(
  _ToastState state,
  int scaledW,
  int scaledH,
  double scale,
) {
  final closeBtnSize = (30 * scale).round();
  state.closeX1 = scaledW - closeBtnSize;
  state.closeY1 = 0;
  state.closeX2 = scaledW;
  state.closeY2 = closeBtnSize;

  final actionH = (42 * scale).round();
  final dividerY = scaledH - actionH;

  state.folderX1 = 0;
  state.folderY1 = dividerY;
  state.folderX2 = scaledW ~/ 2;
  state.folderY2 = scaledH;

  state.fileX1 = scaledW ~/ 2;
  state.fileY1 = dividerY;
  state.fileX2 = scaledW;
  state.fileY2 = scaledH;
}

int _createGdiFont(int height, int weight) {
  final facePtr = 'Segoe UI'.toNativeUtf16();
  try {
    return createFontW(
      -height,
      0,
      0,
      0,
      weight,
      0,
      0,
      0,
      DEFAULT_CHARSET,
      OUT_DEFAULT_PRECIS,
      CLIP_DEFAULT_PRECIS,
      CLEARTYPE_QUALITY,
      DEFAULT_PITCH | FF_DONTCARE,
      facePtr,
    );
  } finally {
    calloc.free(facePtr);
  }
}

void _applyRoundCorners(int hwnd, int w, int h) {
  // Win11 原生圆角
  try {
    final pref = calloc<Int32>();
    try {
      pref.value = DWMWCP_ROUND;
      dwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        pref.cast(),
        sizeOf<Int32>(),
      );
    } finally {
      calloc.free(pref);
    }
  } catch (_) {
    // Win10 不支持
  }
  // 所有版本补充 SetWindowRgn（Win11 上两者互补）
  final hRgn = createRoundRectRgn(0, 0, w, h, 12, 12);
  if (hRgn != 0) {
    setWindowRgn(hwnd, hRgn, 1);
    // SetWindowRgn 接管 hRgn 所有权，无需 DeleteObject
  }
}

// =============================================================================
// 主 Tick — 在 Dart Timer 回调中执行（isolate 已激活）
// =============================================================================

void _processWindowTick(int hwnd, _ToastState state) {
  // ── 1. 更新悬停状态（复用 _sharedCursorPt，避免每 tick malloc/free）──────
  getCursorPos(_sharedCursorPt);
  screenToClient(hwnd, _sharedCursorPt);
  final cx = _sharedCursorPt.ref.x;
  final cy = _sharedCursorPt.ref.y;

  state.isHovered =
      cx >= 0 && cy >= 0 && cx < state.scaledW && cy < state.scaledH;

  final prevButton = state.hoveredButton;
  if (state.isHovered) {
    state.hoveredButton = _hitTest(state, cx, cy);
  } else {
    state.hoveredButton = 0;
  }

  // ── 2. 检测点击（检测下降沿：上次未按 → 本次按下）──────────────────────
  final vkState = getAsyncKeyState(VK_LBUTTON);
  final isCurrentlyDown = (vkState & 0x8000) != 0;
  final wasJustPressed = !state.prevMouseDown && isCurrentlyDown;
  state.prevMouseDown = isCurrentlyDown;

  if (wasJustPressed && state.isHovered && state.phase != _Phase.fadeOut) {
    _handleClick(hwnd, state, cx, cy);
    return; // handleClick 可能已移除 state，直接 return
  }

  // ── 3. 阶段状态机 ──────────────────────────────────────────────────────
  switch (state.phase) {
    case _Phase.fadeIn:
      state.alpha = (state.alpha + 20).clamp(0, 230);
      // alpha 变化只需 SetLayeredWindowAttributes，无需重新绘制 GDI 内容
      setLayeredWindowAttributes(hwnd, 0, state.alpha, LWA_ALPHA);
      if (state.alpha >= 230) {
        state.phase = _Phase.autoClose;
        state.autoCloseStart = DateTime.now();
      }

    case _Phase.autoClose:
      if (state.isHovered) {
        // 悬停时重置倒计时
        state.autoCloseStart = DateTime.now();
      } else {
        final elapsed = DateTime.now().difference(
          state.autoCloseStart ?? DateTime.now(),
        );
        if (elapsed.inSeconds >= 8) {
          state.phase = _Phase.fadeOut;
        }
      }

    case _Phase.fadeOut:
      state.alpha = (state.alpha - 20).clamp(0, 255);
      // alpha 变化只需 SetLayeredWindowAttributes，无需重新绘制 GDI 内容
      setLayeredWindowAttributes(hwnd, 0, state.alpha, LWA_ALPHA);
      if (state.alpha <= 0) {
        _destroyToast(hwnd, state);
        return;
      }
  }

  // ── 4. 仅在悬停按钮变化时重绘（GDI 内容变了），alpha 变化不触发重绘 ──────
  if (state.hoveredButton != prevButton) {
    _repaintWindow(hwnd, state);
  }
}

int _hitTest(_ToastState state, int x, int y) {
  if (x >= state.closeX1 &&
      x < state.closeX2 &&
      y >= state.closeY1 &&
      y < state.closeY2) {
    return 1;
  }
  if (x >= state.folderX1 &&
      x < state.folderX2 &&
      y >= state.folderY1 &&
      y < state.folderY2) {
    return 2;
  }
  if (x >= state.fileX1 &&
      x < state.fileX2 &&
      y >= state.fileY1 &&
      y < state.fileY2) {
    return 3;
  }
  return 0;
}

void _handleClick(int hwnd, _ToastState state, int cx, int cy) {
  final btn = _hitTest(state, cx, cy);
  if (btn == 1) {
    // 关闭
    state.phase = _Phase.fadeOut;
  } else if (btn == 2) {
    // 打开文件夹
    scheduleMicrotask(state.onOpenFolder);
    state.phase = _Phase.fadeOut;
  } else if (btn == 3) {
    // 打开文件
    scheduleMicrotask(state.onOpenFile);
    state.phase = _Phase.fadeOut;
  }
}

void _destroyToast(int hwnd, _ToastState state) {
  _states.remove(hwnd);
  _releaseWindowResources(hwnd, state: state);
  try {
    destroyWindow(hwnd);
  } catch (e) {
    logError(_tag, '_destroyToast: destroyWindow($hwnd) failed', e);
  }
  state.onDismissed();
  logInfo(_tag, 'toast destroyed hwnd=$hwnd');
}

void _releaseWindowResources(int hwnd, {_ToastState? state}) {
  final s = state ?? _states[hwnd];
  if (s == null) return;
  if (s.hFontTitle != 0) {
    deleteObject(s.hFontTitle);
    s.hFontTitle = 0;
  }
  if (s.hFontBody != 0) {
    deleteObject(s.hFontBody);
    s.hFontBody = 0;
  }
}

// =============================================================================
// GDI 绘制（GetDC + 双缓冲，在 Dart Timer 回调中执行）
// =============================================================================

void _repaintWindow(int hwnd, _ToastState state) {
  final hdc = getDC(hwnd);
  if (hdc == 0) return;

  final w = state.scaledW;
  final h = state.scaledH;

  // 双缓冲
  final memDC = createCompatibleDC(hdc);
  final hBitmap = createCompatibleBitmap(hdc, w, h);
  final oldBitmap = selectObject(memDC, hBitmap);

  try {
    final colors = Win32ToastWindow.instance.isDarkMode
        ? _ToastColors.dark
        : _ToastColors.light;
    final scale = h / 130.0;

    _drawBackground(memDC, w, h, colors);
    _drawHeader(memDC, state, w, h, scale, colors);
    _drawContent(memDC, state, w, h, scale, colors);
    _drawDivider(memDC, w, h, scale, colors);
    _drawActions(memDC, state, w, h, scale, colors);

    bitBlt(hdc, 0, 0, w, h, memDC, 0, 0, SRCCOPY);
  } finally {
    selectObject(memDC, oldBitmap);
    deleteObject(hBitmap);
    deleteDC(memDC);
    releaseDC(hwnd, hdc);
  }

  // 防止 DefWindowProcW 在 WM_PAINT 中擦除我们的内容
  validateRect(hwnd, nullptr);
}

void _drawBackground(int dc, int w, int h, _ToastColors colors) {
  final brush = createSolidBrush(colorToCOLORREF(colors.bg));
  final rect = calloc<RECT>();
  try {
    rect.ref
      ..left = 0
      ..top = 0
      ..right = w
      ..bottom = h;
    fillRect(dc, rect, brush);
  } finally {
    calloc.free(rect);
    deleteObject(brush);
  }
}

void _drawHeader(
  int dc,
  _ToastState state,
  int w,
  int h,
  double scale,
  _ToastColors colors,
) {
  final headerH = (36 * scale).round();
  final pad = (14 * scale).round();
  final iconSize = (18 * scale).round();
  final iconX = pad;
  final iconY = (headerH - iconSize) ~/ 2;

  // 绿色方块（简化的勾图标背景）
  final greenBrush = createSolidBrush(colorToCOLORREF(colors.green));
  final iconRect = calloc<RECT>();
  try {
    iconRect.ref
      ..left = iconX
      ..top = iconY
      ..right = iconX + iconSize
      ..bottom = iconY + iconSize;
    fillRect(dc, iconRect, greenBrush);

    // "✓" 文字
    setBkMode(dc, TRANSPARENT);
    setTextColor(dc, colorToCOLORREF(0xFFFFFF));
    final checkFont = _createGdiFont((9 * scale).round(), FW_BOLD);
    final oldFont = selectObject(dc, checkFont);
    final checkPtr = '✓'.toNativeUtf16();
    try {
      drawTextW(
        dc,
        checkPtr,
        -1,
        iconRect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
      );
    } finally {
      calloc.free(checkPtr);
      selectObject(dc, oldFont);
      deleteObject(checkFont);
    }
  } finally {
    calloc.free(iconRect);
    deleteObject(greenBrush);
  }

  // 标题文字
  final titleX = iconX + iconSize + (7 * scale).round();
  final titleRect = calloc<RECT>();
  final titlePtr = state.title.toNativeUtf16();
  try {
    titleRect.ref
      ..left = titleX
      ..top = 0
      ..right = w - (36 * scale).round()
      ..bottom = headerH;
    final oldFont = selectObject(dc, state.hFontTitle);
    setTextColor(dc, colorToCOLORREF(colors.text));
    setBkMode(dc, TRANSPARENT);
    drawTextW(
      dc,
      titlePtr,
      -1,
      titleRect,
      DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
    selectObject(dc, oldFont);
  } finally {
    calloc.free(titleRect);
    calloc.free(titlePtr);
  }

  // 关闭按钮
  final closeSize = (30 * scale).round();
  final closeBtnX = w - closeSize;
  if (state.hoveredButton == 1) {
    final hoverBrush = createSolidBrush(colorToCOLORREF(colors.btnHover));
    final closeRect = calloc<RECT>();
    try {
      closeRect.ref
        ..left = closeBtnX
        ..top = 0
        ..right = w
        ..bottom = closeSize;
      fillRect(dc, closeRect, hoverBrush);
    } finally {
      calloc.free(closeRect);
      deleteObject(hoverBrush);
    }
  }

  final xFont = _createGdiFont((12 * scale).round(), FW_NORMAL);
  final oldXFont = selectObject(dc, xFont);
  setTextColor(
    dc,
    colorToCOLORREF(
      state.hoveredButton == 1 ? colors.text : colors.textMuted,
    ),
  );
  final xRect = calloc<RECT>();
  final xPtr = '✕'.toNativeUtf16();
  try {
    xRect.ref
      ..left = closeBtnX
      ..top = 0
      ..right = w
      ..bottom = closeSize;
    drawTextW(
      dc,
      xPtr,
      -1,
      xRect,
      DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
  } finally {
    calloc.free(xPtr);
    calloc.free(xRect);
    selectObject(dc, oldXFont);
    deleteObject(xFont);
  }
}

void _drawContent(
  int dc,
  _ToastState state,
  int w,
  int h,
  double scale,
  _ToastColors colors,
) {
  final headerH = (36 * scale).round();
  final actionH = (42 * scale).round();
  final dividerY = h - actionH;
  final contentH = dividerY - headerH;
  final pad = (14 * scale).round();

  // EXT 徽章
  final badgeW = (38 * scale).round();
  final badgeH = (38 * scale).round();
  final badgeX = pad;
  final badgeY = headerH + (contentH - badgeH) ~/ 2;

  final badgeBrush = createSolidBrush(colorToCOLORREF(colors.btnHover));
  final badgeRect = calloc<RECT>();
  try {
    badgeRect.ref
      ..left = badgeX
      ..top = badgeY
      ..right = badgeX + badgeW
      ..bottom = badgeY + badgeH;
    fillRect(dc, badgeRect, badgeBrush);

    final extFont = _createGdiFont((10 * scale).round(), FW_SEMIBOLD);
    final oldExtFont = selectObject(dc, extFont);
    setBkMode(dc, TRANSPARENT);
    setTextColor(dc, colorToCOLORREF(colors.accent));
    final extPtr = state.fileExt.toNativeUtf16();
    try {
      drawTextW(
        dc,
        extPtr,
        -1,
        badgeRect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
      );
    } finally {
      calloc.free(extPtr);
      selectObject(dc, oldExtFont);
      deleteObject(extFont);
    }
  } finally {
    calloc.free(badgeRect);
    deleteObject(badgeBrush);
  }

  // 文件名
  final textX = badgeX + badgeW + (10 * scale).round();
  final textW = w - textX - pad;
  final nameRect = calloc<RECT>();
  final namePtr = state.body.toNativeUtf16();
  try {
    nameRect.ref
      ..left = textX
      ..top = badgeY
      ..right = textX + textW
      ..bottom = badgeY + badgeH;
    final oldFont = selectObject(dc, state.hFontBody);
    setTextColor(dc, colorToCOLORREF(colors.text));
    setBkMode(dc, TRANSPARENT);
    drawTextW(
      dc,
      namePtr,
      -1,
      nameRect,
      DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
    selectObject(dc, oldFont);
  } finally {
    calloc.free(namePtr);
    calloc.free(nameRect);
  }
}

void _drawDivider(
  int dc,
  int w,
  int h,
  double scale,
  _ToastColors colors,
) {
  final actionH = (42 * scale).round();
  final dividerY = h - actionH;
  final divBrush = createSolidBrush(colorToCOLORREF(colors.divider));
  final divRect = calloc<RECT>();
  try {
    divRect.ref
      ..left = 0
      ..top = dividerY
      ..right = w
      ..bottom = dividerY + 1;
    fillRect(dc, divRect, divBrush);
  } finally {
    calloc.free(divRect);
    deleteObject(divBrush);
  }
}

void _drawActions(
  int dc,
  _ToastState state,
  int w,
  int h,
  double scale,
  _ToastColors colors,
) {
  final actionH = (42 * scale).round();
  final dividerY = h - actionH;
  final halfW = w ~/ 2;

  // 打开文件夹（左半）
  if (state.hoveredButton == 2) {
    final hoverBrush = createSolidBrush(colorToCOLORREF(colors.btnHover));
    final rect = calloc<RECT>();
    try {
      rect.ref
        ..left = 0
        ..top = dividerY
        ..right = halfW
        ..bottom = h;
      fillRect(dc, rect, hoverBrush);
    } finally {
      calloc.free(rect);
      deleteObject(hoverBrush);
    }
  }

  // 打开文件（右半，accent 背景）
  final fileBg =
      state.hoveredButton == 3
          ? (Win32ToastWindow.instance.isDarkMode ? 0x3A7FCC : 0x0055AA)
          : colors.accent;
  final fileBrush = createSolidBrush(colorToCOLORREF(fileBg));
  final fileRect = calloc<RECT>();
  try {
    fileRect.ref
      ..left = halfW
      ..top = dividerY
      ..right = w
      ..bottom = h;
    fillRect(dc, fileRect, fileBrush);
  } finally {
    calloc.free(fileRect);
    deleteObject(fileBrush);
  }

  // 文字
  final btnFont = _createGdiFont((11 * scale).round(), FW_NORMAL);
  final oldFont = selectObject(dc, btnFont);
  setBkMode(dc, TRANSPARENT);

  final folderRect = calloc<RECT>();
  final folderPtr = '打开文件夹'.toNativeUtf16();
  try {
    folderRect.ref
      ..left = 0
      ..top = dividerY
      ..right = halfW
      ..bottom = h;
    setTextColor(dc, colorToCOLORREF(colors.text));
    drawTextW(
      dc,
      folderPtr,
      -1,
      folderRect,
      DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
  } finally {
    calloc.free(folderPtr);
    calloc.free(folderRect);
  }

  final fileTextRect = calloc<RECT>();
  final fileTextPtr = '打开文件'.toNativeUtf16();
  try {
    fileTextRect.ref
      ..left = halfW
      ..top = dividerY
      ..right = w
      ..bottom = h;
    setTextColor(dc, colorToCOLORREF(0xFFFFFF));
    drawTextW(
      dc,
      fileTextPtr,
      -1,
      fileTextRect,
      DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
  } finally {
    calloc.free(fileTextPtr);
    calloc.free(fileTextRect);
  }

  selectObject(dc, oldFont);
  deleteObject(btnFont);
}
