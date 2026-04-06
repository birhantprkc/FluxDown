import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';
import 'package:url_launcher/url_launcher.dart';

/// 在系统默认文件管理器中打开文件所在目录。
/// 兼容 Windows / macOS / Linux，尊重用户注册的默认文件管理器。
///
/// [filePath] 可以是文件路径，也可以是目录路径：
/// - 文件路径 → 打开其所在目录
/// - 目录路径 → 直接打开该目录
Future<void> openFolder(String filePath) async {
  // 判断传入的路径是文件还是目录，避免对目录路径错误地取 parent。
  // 当通知回调中 payload 丢失而 fallback 到 _resolveDefaultDir() 时，
  // 传入的是目录路径；若仍对其取 .parent，就会向上跳一级（如打开用户目录）。
  final type = await FileSystemEntity.type(filePath);
  final String dir;
  switch (type) {
    case FileSystemEntityType.file:
      dir = File(filePath).parent.path;
    case FileSystemEntityType.directory:
      dir = filePath;
    default:
      // 路径不存在 — 通过最后一段是否含扩展名来推测：
      // 有扩展名视为文件路径取 parent；否则视为目录路径直接使用。
      final leaf = filePath.split(Platform.pathSeparator).last;
      dir = leaf.contains('.') ? File(filePath).parent.path : filePath;
  }

  if (Platform.isWindows) {
    // 直接调用 ShellExecuteW("open", dir)，两全其美：
    //
    // 1. 尊重第三方文件管理器 — ShellExecuteW 对目录路径执行 "open" 动词时，
    //    会查找 HKCR\Directory\shell\open\command 中注册的处理程序
    //    （如 OneCommander、Files、Directory Opus 等），与用户双击文件夹行为一致。
    //
    // 2. 避免 cmd /c start 的命令行重解析 bug — cmd 会对参数做二次 tokenize，
    //    路径含空格、CJK 字符或尾部反斜杠时（ `\"` 被当作转义引号），
    //    单条路径可能被拆分为多个参数，导致同时打开两个资源管理器窗口。
    //
    // 3. 不走 url_launcher 的 file:// URI — url_launcher 在 Windows 上对
    //    file:// URI 硬编码路由给 explorer.exe，同样会绕过第三方文件管理器。
    _shellExecuteOpen(dir);
  } else {
    await launchUrl(Uri.file(dir));
  }
}

/// 用系统默认程序打开文件。
/// 兼容 Windows / macOS / Linux。
Future<void> openFile(String filePath) async {
  // 所有平台统一使用 url_launcher 的 file:// URI。
  // 在 Windows 上 launchUrl(file://) 最终调用 ShellExecuteW("open", ...)，
  // 通过完整的注册表查找链（HKCR → UserChoice → OpenWithProgids）解析关联应用，
  // 比 cmd /c start 更可靠——后者对 .zip/.7z/.docx 等通过现代 Windows 设置
  // 注册的文件类型偶尔无法正确识别关联程序。
  await launchUrl(Uri.file(filePath));
}

// ---------------------------------------------------------------------------
// Win32 FFI — ShellExecuteW（仅 Windows 平台）
// ---------------------------------------------------------------------------
//
// MSDN 签名:
// HINSTANCE ShellExecuteW(
//   HWND    hwnd,          // 父窗口句柄（NULL = 无）
//   LPCWSTR lpOperation,   // 动词（"open"）
//   LPCWSTR lpFile,        // 目标路径
//   LPCWSTR lpParameters,  // 参数（NULL）
//   LPCWSTR lpDirectory,   // 工作目录（NULL）
//   INT     nShowCmd,      // 显示方式（SW_SHOWNORMAL = 1）
// );
//
// 路径以 UTF-16 宽字符传入，不经过任何 shell 命令行解析，
// 因此空格、CJK 字符、特殊符号均无需额外转义。
//
// 返回值 > 32 表示成功；<= 32 为错误码（此处忽略，静默降级）。
// ---------------------------------------------------------------------------

typedef _ShellExecuteWNative = IntPtr Function(
  IntPtr hwnd,
  Pointer<Utf16> lpOperation,
  Pointer<Utf16> lpFile,
  Pointer<Utf16> lpParameters,
  Pointer<Utf16> lpDirectory,
  Int32 nShowCmd,
);

typedef _ShellExecuteWDart = int Function(
  int hwnd,
  Pointer<Utf16> lpOperation,
  Pointer<Utf16> lpFile,
  Pointer<Utf16> lpParameters,
  Pointer<Utf16> lpDirectory,
  int nShowCmd,
);

const int _swShowNormal = 1;

/// 延迟加载的 ShellExecuteW 函数指针。
///
/// 使用函数内部 `late` 变量而非顶层 `final`，确保在 macOS/Linux 上
/// 即使意外调用也能给出清晰错误信息，而不是在模块加载时触发
/// `DynamicLibrary.open('shell32.dll')` 导致不可控的 crash。
final _ShellExecuteWDart _shellExecuteW = () {
  assert(Platform.isWindows, 'ShellExecuteW FFI must only be called on Windows');
  final shell32 = DynamicLibrary.open('shell32.dll');
  return shell32
      .lookupFunction<_ShellExecuteWNative, _ShellExecuteWDart>('ShellExecuteW');
}();

/// 调用 Win32 ShellExecuteW 以 "open" 动词打开路径。
void _shellExecuteOpen(String path) {
  // 显式指定 allocator: calloc，确保与 calloc.free() 配对使用同一分配器。
  // package:ffi 的 toNativeUtf16() 默认使用 malloc，若用 calloc.free() 释放
  // 属于未定义行为（Windows 上 malloc/calloc 碰巧共享同一进程堆，不会 crash，
  // 但其他平台或未来版本不保证）。
  final operation = 'open'.toNativeUtf16(allocator: calloc);
  final filePath = path.toNativeUtf16(allocator: calloc);
  try {
    _shellExecuteW(
      0, // hwnd = NULL
      operation,
      filePath,
      nullptr, // lpParameters = NULL
      nullptr, // lpDirectory = NULL
      _swShowNormal,
    );
  } finally {
    calloc.free(operation);
    calloc.free(filePath);
  }
}
