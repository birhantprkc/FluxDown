import 'dart:async';
import 'dart:io';

import 'package:file_picker/file_picker.dart';

/// 封装 [FilePicker] 调用，解决以下跨平台已知问题：
///
/// **Linux**
/// - file_picker v10.3+ 优先使用 XDG Desktop Portal（D-Bus），
///   fallback 到 `kdialog`/`zenity`/`qarma`。
/// - Portal 覆盖 GNOME、KDE、Sway、Hyprland 等主流桌面/WM。
/// - `lockParentWindow` 在 Linux 实现中被忽略（无实际效果）。
///
/// **Windows**
/// - `getDirectoryPath` 使用 `IFileDialog` COM 接口，在 `compute()` 后台
///   isolate 中运行。`lockParentWindow` 通过 `GetForegroundWindow()` 获取
///   父窗口句柄，但在 isolate 中执行时前景窗口可能已不是 Flutter 窗口，
///   导致对话框绑定到错误窗口而**不可见** — 用户以为什么都没发生。
/// - `pickFiles` / `saveFile` 使用 `FindWindowA('FLUTTER_RUNNER_WIN32_WINDOW')`
///   查找窗口，相对可靠。
/// - COM `CoInitializeEx(COINIT_APARTMENTTHREADED)` 可能在复用的 Dart VM
///   线程上遭遇已有的 `COINIT_MULTITHREADED`，导致
///   `RPC_E_CHANGED_MODE (0x80010106)` 异常。
///
/// **本服务的缓解策略**
/// 1. Windows `getDirectoryPath` 禁用 `lockParentWindow`，避免
///    `GetForegroundWindow()` 竞态导致对话框不可见。
/// 2. Windows 上 COM 错误立即重试一次（线程池分配到新线程后通常可恢复）。
/// 3. 捕获所有异常并以 [FilePickerException] 向上抛出，失败立即返回。
/// 4. 调用前短暂 yield，让 Flutter 完成当前帧渲染。
class FilePickerService {
  FilePickerService._();

  /// 最大重试次数（仅 Windows COM 错误时重试）。
  static const _maxRetries = 1;

  // ─────────────────────────────────────────────
  // 公开 API
  // ─────────────────────────────────────────────

  /// 选取保存目录。
  ///
  /// 返回用户选中的目录路径，用户取消时返回 `null`。
  /// 失败时抛出 [FilePickerException]。
  static Future<String?> pickDirectory({
    required String dialogTitle,
    String? initialDirectory,
  }) async {
    await _preCallDelay();

    // Windows: getDirectoryPath 在 compute isolate 中用
    // GetForegroundWindow() 获取父窗口句柄，isolate 切换后前景窗口
    // 已不是 Flutter 窗口，会导致对话框绑定到错误窗口而不可见。
    // 禁用 lockParentWindow 让对话框作为独立顶层窗口弹出。
    final lock = !Platform.isWindows;

    return _withRetry(() async {
      return await FilePicker.platform.getDirectoryPath(
        dialogTitle: dialogTitle,
        lockParentWindow: lock,
        initialDirectory: initialDirectory,
      );
    });
  }

  /// 选取单个或多个文件。
  ///
  /// 返回 [FilePickerResult]，用户取消时返回 `null`。
  /// 失败时抛出 [FilePickerException]。
  static Future<FilePickerResult?> pickFiles({
    required String dialogTitle,
    FileType type = FileType.any,
    List<String>? allowedExtensions,
    bool allowMultiple = false,
  }) async {
    await _preCallDelay();

    // pickFiles 在 Windows 上使用 FindWindowA('FLUTTER_RUNNER_WIN32_WINDOW')
    // 查找父窗口，比 GetForegroundWindow 可靠，可以启用 lock。
    return _withRetry(() async {
      return await FilePicker.platform.pickFiles(
        dialogTitle: dialogTitle,
        type: type,
        allowedExtensions: allowedExtensions,
        allowMultiple: allowMultiple,
        lockParentWindow: true,
      );
    });
  }

  /// 保存文件对话框。
  ///
  /// 返回用户选择的保存路径，用户取消时返回 `null`。
  /// 失败时抛出 [FilePickerException]。
  static Future<String?> saveFile({
    required String dialogTitle,
    String? fileName,
    String? initialDirectory,
    FileType type = FileType.any,
    List<String>? allowedExtensions,
  }) async {
    await _preCallDelay();

    return _withRetry(() async {
      return await FilePicker.platform.saveFile(
        dialogTitle: dialogTitle,
        fileName: fileName,
        initialDirectory: initialDirectory,
        type: type,
        allowedExtensions: allowedExtensions,
        lockParentWindow: true,
      );
    });
  }

  // ─────────────────────────────────────────────
  // 内部辅助
  // ─────────────────────────────────────────────

  /// 带重试的调用包装。
  ///
  /// Windows 上 COM 初始化冲突是临时性错误（线程池分配到已被其他插件
  /// 初始化为 COINIT_MULTITHREADED 的线程），重试一次通常可分配到新线程。
  /// 非 COM 错误或非 Windows 平台不重试，立即抛出。
  static Future<T?> _withRetry<T>(Future<T?> Function() fn) async {
    Object? lastError;

    for (var attempt = 0; attempt <= _maxRetries; attempt++) {
      try {
        return await fn();
      } on FilePickerException {
        rethrow;
      } catch (e) {
        lastError = e;
        final reason = _classifyError(e);

        // 仅 Windows COM 错误值得重试
        if (reason == FilePickerFailReason.comInitFailed &&
            attempt < _maxRetries) {
          // 短暂等待让线程池有机会分配新线程
          await Future<void>.delayed(const Duration(milliseconds: 100));
          continue;
        }

        throw FilePickerException(reason, cause: e);
      }
    }

    // 理论上不可达，但防御性处理
    throw FilePickerException(
      _classifyError(lastError ?? 'unknown'),
      cause: lastError,
    );
  }

  /// 调用前的微小延迟：
  /// - 让 Flutter 完成当前帧（按钮状态更新等），避免在渲染未提交时
  ///   就启动耗时的 isolate/子进程调用。
  /// - Linux Wayland：给 compositor 机会先处理当前帧的 surface commit，
  ///   稍微提高 dialog 进程获得焦点的概率。
  /// - Windows：确保主 UI 线程已完成帧渲染后再进入 compute isolate，
  ///   此时 GetForegroundWindow() 更有可能返回 Flutter 窗口句柄。
  static Future<void> _preCallDelay() async {
    // 两帧时间（~32 ms），让 Flutter 先提交按钮 disabled 状态的渲染
    await Future<void>.delayed(const Duration(milliseconds: 32));
  }

  /// 将底层异常映射到 [FilePickerFailReason]。
  static FilePickerFailReason _classifyError(Object e) {
    final msg = e.toString().toLowerCase();

    // Windows: COM 线程模型冲突
    // RPC_E_CHANGED_MODE = 0x80010106
    if (msg.contains('80010106') ||
        msg.contains('rpc_e_changed_mode') ||
        msg.contains('coinitialize')) {
      return FilePickerFailReason.comInitFailed;
    }

    // Linux: XDG Desktop Portal D-Bus 连接失败
    if (msg.contains('dbus') ||
        msg.contains('portal') ||
        msg.contains('org.freedesktop')) {
      return FilePickerFailReason.noDialogTool;
    }

    // Linux: 找不到 kdialog/zenity/qarma（Portal fallback 后的最终失败）
    if (msg.contains('executable') ||
        msg.contains('kdialog') ||
        msg.contains('zenity') ||
        msg.contains('qarma') ||
        msg.contains('which') ||
        (Platform.isLinux && msg.contains('exception'))) {
      return FilePickerFailReason.noDialogTool;
    }

    // Windows: HRESULT 失败（dialog show 失败等）
    if (msg.contains('windowsexception') ||
        msg.contains('hresult') ||
        msg.contains('hr =')) {
      return FilePickerFailReason.nativeDialogFailed;
    }

    return FilePickerFailReason.unknown;
  }
}

// ─────────────────────────────────────────────
// 异常类型
// ─────────────────────────────────────────────

/// file picker 操作失败的原因分类。
enum FilePickerFailReason {
  /// 操作超时（dialog 在后台挂起、用户长时间未操作等）
  timeout,

  /// Linux 上找不到文件选择工具（Portal / kdialog / zenity / qarma 均不可用）
  noDialogTool,

  /// Windows COM 初始化失败（线程模型冲突）
  comInitFailed,

  /// 原生对话框调用失败（HRESULT 错误等）
  nativeDialogFailed,

  /// 未归类的未知错误
  unknown,
}

/// [FilePickerService] 抛出的统一异常。
class FilePickerException implements Exception {
  const FilePickerException(this.reason, {this.cause});

  final FilePickerFailReason reason;

  /// 原始异常（可为 null）
  final Object? cause;

  @override
  String toString() => 'FilePickerException($reason, cause: $cause)';
}
