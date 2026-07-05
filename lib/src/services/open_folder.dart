import '../bindings/bindings.dart';

/// 在文件管理器中打开文件所在目录（尽可能选中文件）或目录本身。
///
/// 实际实现完全在 Rust 端：见 native/hub/src/reveal_file.rs。
/// Rust 端会按以下顺序决定：
///   1. 用户在设置中配置了自定义命令模板（reveal_file_cmd / open_dir_cmd）
///      → 走模板（cmd /c 或 sh -c），支持任意第三方文件管理器
///   2. 否则走平台默认：
///      Windows: 文件→第三方默认 FM 打开父目录，否则 explorer /select；目录→cmd /c start
///      macOS:   open -R 或 open
///      Linux:   D-Bus FileManager1.ShowItems 或 xdg-open
///
/// [filePath] 可以是文件路径或目录路径——Rust 端会用 fs::metadata 自动判定。
Future<void> openFolder(String filePath) async {
  RevealFile(path: filePath).sendSignalToRust();
}

/// 用系统默认程序打开文件。
///
/// 交给 Rust 端以**裸路径**经 shell 打开（Windows `explorer.exe` / macOS `open`
/// / Linux `xdg-open`），正确解析扩展名关联，包括 .mp4 等由 UWP/Store 应用处理
/// 的类型。此前用 `launchUrl(Uri.file())` 传 `file://` URL，ShellExecute 无法据此
/// 激活 UWP 关联应用，导致这类文件“点开没反应”。实现见 native/hub/src/reveal_file.rs。
Future<void> openFile(String filePath) async {
  OpenFile(path: filePath).sendSignalToRust();
}
