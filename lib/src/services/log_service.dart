import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';
import 'package:path/path.dart' as p;

/// 文件日志服务 — 将日志写入 exe 同级 logs/ 目录，按日期分文件。
///
/// 使用缓冲写入 + 定时刷盘，兼顾性能和崩溃前日志完整度。
/// 单例，应在 app 启动最早期调用 [init]。
class LogService {
  LogService._();
  static final LogService instance = LogService._();

  RandomAccessFile? _raf;
  String? _currentDateTag;
  Timer? _flushTimer;
  bool _initialized = false;

  /// 自上次 flush 以来是否有新数据写入
  bool _dirty = false;

  /// 日志保留天数
  static const int _retentionDays = 7;

  /// 日志目录
  late final Directory _logDir;

  /// 暴露日志目录路径，供导出日志等功能使用。
  Directory get logDir => _logDir;

  /// 初始化日志服务。应在 main() 最开始调用。
  void init() {
    if (_initialized) return;
    _initialized = true;

    _logDir = _resolveLogDir();
    if (!_logDir.existsSync()) {
      _logDir.createSync(recursive: true);
    }

    _rotateSink();

    // 启动时清理 7 天前的旧日志文件
    _cleanupOldLogs();

    // 每 2 秒刷盘一次，确保崩溃前有足够日志。
    // 仅在有新数据写入时才调用 flushSync，避免空闲时无谓的磁盘 I/O。
    _flushTimer = Timer.periodic(const Duration(seconds: 2), (_) {
      if (!_dirty) return;
      try {
        _raf?.flushSync();
        _dirty = false;
      } catch (_) {}
    });
  }

  /// 写一条日志。[tag] 是模块标签，[message] 是内容。
  void log(String tag, String message) {
    if (!_initialized) return;
    try {
      _rotateSink();
      final now = DateTime.now();
      final ts =
          '${_pad2(now.hour)}:${_pad2(now.minute)}:${_pad2(now.second)}.${_pad3(now.millisecond)}';
      final line = '$ts [$tag] $message\n';
      _raf?.writeStringSync(line);
      _dirty = true;
      // 仅在 debug 模式下输出到控制台，避免 release 模式的字符串缓存开销
      if (kDebugMode) {
        // ignore: avoid_print
        print(line.trimRight());
      }
    } catch (e) {
      // 日志服务本身不应该抛异常影响业务
      // ignore: avoid_print
      print('[LogService] write error: $e');
    }
  }

  /// 记录错误（含堆栈）
  void error(String tag, String message, [Object? err, StackTrace? stack]) {
    log(tag, 'ERROR: $message');
    if (err != null) log(tag, '  exception: $err');
    if (stack != null) log(tag, '  stackTrace:\n$stack');
    // 错误立即刷盘
    try {
      _raf?.flushSync();
      _dirty = false;
    } catch (_) {}
  }

  /// 将所有日志文件打包为 ZIP 压缩包保存到 [zipPath]。
  ///
  /// 打包前会先刷盘，确保最新日志已写入文件。
  /// [sanitize] 为 true（默认）时，导出前对日志内容进行脱敏处理，
  /// 移除 URL 认证凭证、代理密码、用户路径、设备 ID 等敏感信息。
  /// 返回打包的文件数量。
  Future<int> exportLogs(String zipPath, {bool sanitize = true}) async {
    // 先刷盘，确保最新日志已写入
    try {
      _raf?.flushSync();
      _dirty = false;
    } catch (_) {}

    if (!_logDir.existsSync()) return 0;

    final logFiles = <File>[];
    for (final entity in _logDir.listSync()) {
      if (entity is! File) continue;
      final name = p.basename(entity.path);
      if (!name.startsWith('fluxdown_') || !name.endsWith('.log')) continue;
      logFiles.add(entity);
    }
    if (logFiles.isEmpty) return 0;

    // 按文件名排序（即按日期排序）
    logFiles.sort((a, b) => p.basename(a.path).compareTo(p.basename(b.path)));

    final zipBytes = _buildZip(logFiles, sanitize: sanitize);
    await File(zipPath).writeAsBytes(zipBytes);
    return logFiles.length;
  }

  /// 计算日志目录的总大小（字节）。
  int get logDirSizeBytes {
    if (!_logDir.existsSync()) return 0;
    int total = 0;
    for (final entity in _logDir.listSync()) {
      if (entity is! File) continue;
      final name = p.basename(entity.path);
      if (!name.startsWith('fluxdown_') || !name.endsWith('.log')) continue;
      try {
        total += entity.lengthSync();
      } catch (_) {}
    }
    return total;
  }

  /// 日志文件数量。
  int get logFileCount {
    if (!_logDir.existsSync()) return 0;
    int count = 0;
    for (final entity in _logDir.listSync()) {
      if (entity is! File) continue;
      final name = p.basename(entity.path);
      if (!name.startsWith('fluxdown_') || !name.endsWith('.log')) continue;
      count++;
    }
    return count;
  }

  /// 关闭日志服务
  Future<void> dispose() async {
    _flushTimer?.cancel();
    _flushTimer = null;
    try {
      _raf?.flushSync();
      _raf?.closeSync();
    } catch (_) {}
    _raf = null;
    _initialized = false;
  }

  // ── 内部 ──

  /// 按日期切换日志文件（全同步，无 IOSink 异步问题）
  void _rotateSink() {
    final now = DateTime.now();
    final dateTag = '${now.year}-${_pad2(now.month)}-${_pad2(now.day)}';
    if (dateTag == _currentDateTag && _raf != null) return;

    // 关闭旧文件
    try {
      _raf?.flushSync();
      _raf?.closeSync();
    } catch (_) {}

    _currentDateTag = dateTag;
    final file = File(
      '${_logDir.path}${Platform.pathSeparator}fluxdown_$dateTag.log',
    );
    _raf = file.openSync(mode: FileMode.append);

    final header =
        '\n'
        '====== FluxDown log session started at $now ======\n'
        '  pid: $pid\n'
        '  exe: ${Platform.resolvedExecutable}\n'
        '  isolate: ${Isolate.current.debugName}\n'
        '\n';
    _raf!.writeStringSync(header);
    _dirty = true;
  }

  /// 解析日志目录：
  /// - Linux: ~/.local/share/fluxdown/logs（XDG_DATA_HOME 优先）
  /// - macOS: ~/Library/Application Support/fluxdown/logs
  /// - 其他: exe 同级 logs/
  static Directory _resolveLogDir() {
    if (Platform.isLinux) {
      final xdgData =
          Platform.environment['XDG_DATA_HOME'] ??
          '${Platform.environment['HOME']}/.local/share';
      return Directory('$xdgData/fluxdown/logs');
    }
    if (Platform.isMacOS) {
      final home = Platform.environment['HOME'] ?? '';
      return Directory('$home/Library/Application Support/fluxdown/logs');
    }
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    return Directory('$exeDir${Platform.pathSeparator}logs');
  }

  /// 清理超过 [_retentionDays] 天的 fluxdown_*.log 文件。
  void _cleanupOldLogs() {
    try {
      if (!_logDir.existsSync()) return;
      final cutoff = DateTime.now().subtract(Duration(days: _retentionDays));
      for (final entity in _logDir.listSync()) {
        if (entity is! File) continue;
        final name = entity.uri.pathSegments.last;
        if (!name.startsWith('fluxdown_') || !name.endsWith('.log')) continue;
        try {
          final modified = entity.lastModifiedSync();
          if (modified.isBefore(cutoff)) {
            entity.deleteSync();
          }
        } catch (_) {
          // 单个文件清理失败不影响其他文件
        }
      }
    } catch (_) {
      // 清理失败不影响日志服务正常运行
    }
  }

  static String _pad2(int n) => n.toString().padLeft(2, '0');
  static String _pad3(int n) => n.toString().padLeft(3, '0');
}

/// 全局快捷方法
void logInfo(String tag, String message) =>
    LogService.instance.log(tag, message);

void logError(String tag, String message, [Object? err, StackTrace? stack]) =>
    LogService.instance.error(tag, message, err, stack);

// ══════════════════════════════════════════════════
//  ZIP 构建（纯 Dart 标准库，零外部依赖）
// ══════════════════════════════════════════════════

final List<int> _crc32Table = () {
  final table = List<int>.filled(256, 0);
  for (int i = 0; i < 256; i++) {
    int c = i;
    for (int j = 0; j < 8; j++) {
      c = (c & 1) != 0 ? (0xEDB88320 ^ (c >>> 1)) : (c >>> 1);
    }
    table[i] = c;
  }
  return table;
}();

int _crc32(List<int> data) {
  int crc = 0xFFFFFFFF;
  for (final b in data) {
    crc = _crc32Table[(crc ^ b) & 0xFF] ^ (crc >>> 8);
  }
  return (crc ^ 0xFFFFFFFF) & 0xFFFFFFFF;
}

void _writeU16(BytesBuilder b, int v) {
  b.addByte(v & 0xFF);
  b.addByte((v >> 8) & 0xFF);
}

void _writeU32(BytesBuilder b, int v) {
  b.addByte(v & 0xFF);
  b.addByte((v >> 8) & 0xFF);
  b.addByte((v >> 16) & 0xFF);
  b.addByte((v >> 24) & 0xFF);
}

class _ZipCentralEntry {
  final List<int> nameBytes;
  final int crc;
  final int compressedSize;
  final int uncompressedSize;
  final int localOffset;
  final int dosTime;
  final int dosDate;

  _ZipCentralEntry({
    required this.nameBytes,
    required this.crc,
    required this.compressedSize,
    required this.uncompressedSize,
    required this.localOffset,
    required this.dosTime,
    required this.dosDate,
  });
}

Uint8List _buildZip(List<File> files, {bool sanitize = false}) {
  final out = BytesBuilder(copy: false);
  final centralEntries = <_ZipCentralEntry>[];

  for (final file in files) {
    final name = p.basename(file.path);
    final nameBytes = utf8.encode(name);
    final rawData = file.readAsBytesSync();
    // 脱敏处理：在压缩前对文本内容替换敏感信息
    final data = sanitize ? _sanitizeLogBytes(rawData) : rawData;
    final crc = _crc32(data);
    final compressed = ZLibEncoder(raw: true, level: 6).convert(data);

    // DOS 日期时间
    final mod = file.lastModifiedSync();
    final dosTime = (mod.hour << 11) | (mod.minute << 5) | (mod.second ~/ 2);
    final dosDate = ((mod.year - 1980) << 9) | (mod.month << 5) | mod.day;

    final localOffset = out.length;

    // Local file header
    _writeU32(out, 0x04034b50); // signature
    _writeU16(out, 20); // version needed
    _writeU16(out, 0); // flags
    _writeU16(out, 8); // compression: deflate
    _writeU16(out, dosTime);
    _writeU16(out, dosDate);
    _writeU32(out, crc);
    _writeU32(out, compressed.length);
    _writeU32(out, data.length); // 使用脱敏后的实际大小
    _writeU16(out, nameBytes.length);
    _writeU16(out, 0); // extra length
    out.add(nameBytes);
    out.add(compressed);

    centralEntries.add(
      _ZipCentralEntry(
        nameBytes: nameBytes,
        crc: crc,
        compressedSize: compressed.length,
        uncompressedSize: data.length, // 使用脱敏后的实际大小
        localOffset: localOffset,
        dosTime: dosTime,
        dosDate: dosDate,
      ),
    );
  }

  final centralStart = out.length;

  for (final e in centralEntries) {
    _writeU32(out, 0x02014b50); // signature
    _writeU16(out, 20); // version made by
    _writeU16(out, 20); // version needed
    _writeU16(out, 0); // flags
    _writeU16(out, 8); // compression: deflate
    _writeU16(out, e.dosTime);
    _writeU16(out, e.dosDate);
    _writeU32(out, e.crc);
    _writeU32(out, e.compressedSize);
    _writeU32(out, e.uncompressedSize);
    _writeU16(out, e.nameBytes.length);
    _writeU16(out, 0); // extra length
    _writeU16(out, 0); // comment length
    _writeU16(out, 0); // disk number
    _writeU16(out, 0); // internal attributes
    _writeU32(out, 0); // external attributes
    _writeU32(out, e.localOffset);
    out.add(e.nameBytes);
  }

  final centralSize = out.length - centralStart;

  // End of central directory
  _writeU32(out, 0x06054b50);
  _writeU16(out, 0); // disk number
  _writeU16(out, 0); // central dir start disk
  _writeU16(out, centralEntries.length);
  _writeU16(out, centralEntries.length);
  _writeU32(out, centralSize);
  _writeU32(out, centralStart);
  _writeU16(out, 0); // comment length

  return out.toBytes();
}

// ══════════════════════════════════════════════════
//  日志脱敏（导出时保护敏感信息）
// ══════════════════════════════════════════════════

/// 脱敏规则列表。
///
/// 按顺序应用，每条规则包含一个正则和替换函数。
/// 规则覆盖范围：
///   1. URL 内嵌认证凭证（user:pass@host）
///   2. HTTP(S) URL 长 query string（CDN 签名、学术数据库 token 等）
///   3. Cookie 头值
///   4. Authorization 头值
///   5. 代理用户名/密码字段
///   6. Linux 用户主目录路径
///   7. Windows 用户目录路径
///   8. Analytics 设备 ID（UUID）
final _kSanitizeRules = <({RegExp pattern, String Function(Match m) replace})>[
  // 1. URL 内嵌认证凭证：scheme://user:pass@host → scheme://***@host
  //    覆盖：ftp://user:pass@host/path、http://admin:secret@proxy:8080
  (
    pattern: RegExp(r'([\w+\-.]+://)[^:/\s@]+:[^@\s]+@', caseSensitive: false),
    replace: (m) => '${m[1]}***@',
  ),

  // 2. HTTP(S) URL 长 query string（>50 字符）→ ?[QUERY_REDACTED]
  //    覆盖：知网/百度网盘签名 URL、CDN 防盗链、私人 BT tracker passkey
  //    使用非贪婪 + 向前看，不消耗 URL 后面的分隔符（逗号、括号等）
  (
    pattern: RegExp(
      r'(https?://[^?\s]{3,})\?([^\s]{50,}?)(?=[\s,)\]>]|$)',
      caseSensitive: false,
      multiLine: true,
    ),
    replace: (m) => '${m[1]}?[QUERY_REDACTED]',
  ),

  // 3. Cookie 头值：Cookie: <value> → Cookie: [REDACTED]
  (
    pattern: RegExp(r'(cookie\b[^:]*:\s*)\S+', caseSensitive: false),
    replace: (m) => '${m[1]}[REDACTED]',
  ),

  // 4. Authorization 头值：Authorization: Bearer <token> → Authorization: [REDACTED]
  //    覆盖：Bearer Token、Basic 认证、API Key 等两段式头值
  //    (?:\S+\s+)? 可选匹配 scheme（如 "Bearer "），\S+ 匹配实际凭证
  (
    pattern: RegExp(
      r'(authorization\b[^:]*:\s*)(?:\S+\s+)?\S+',
      caseSensitive: false,
    ),
    replace: (m) => '${m[1]}[REDACTED]',
  ),

  // 5. 代理用户名/密码字段（非空值）
  //    覆盖：Settings 日志 `config: proxy_password=xxx`
  //          actor 日志 `proxy config changed: proxy_password=xxx`
  (
    pattern: RegExp(
      r'(proxy[_\s]?(?:password|username)\s*[=:]\s*)\S+',
      caseSensitive: false,
    ),
    replace: (m) => '${m[1]}[REDACTED]',
  ),

  // 6. Linux 用户主目录：/home/username/ → /home/***/
  //    覆盖：save_dir、temp/dest 路径、exe 路径、图标路径
  (pattern: RegExp(r'/home/[^/\s]+/'), replace: (_) => '/home/***/'),

  // 7. Windows 用户目录：C:\Users\username\ → C:\Users\***\
  (
    pattern: RegExp(r'([A-Za-z]:\\[Uu]sers\\)[^\\\s]+\\'),
    replace: (m) => '${m[1]}***\\',
  ),

  // 8. Analytics 设备 ID：deviceId=xxxxxxxx-... → deviceId=[REDACTED]
  //    覆盖：[Analytics] initialized, consent=true, deviceId=35e2c0fd-...
  (
    pattern: RegExp(
      r'(deviceId=)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',
      caseSensitive: false,
    ),
    replace: (m) => '${m[1]}[REDACTED]',
  ),
];

/// 对日志内容应用全部脱敏规则，返回脱敏后的文本。
String _sanitizeLogContent(String content) {
  for (final rule in _kSanitizeRules) {
    content = content.replaceAllMapped(rule.pattern, rule.replace);
  }
  return content;
}

/// 对日志字节内容进行脱敏，返回脱敏后的字节。
///
/// 处理流程：UTF-8 解码（allowMalformed）→ 正则替换 → UTF-8 编码
Uint8List _sanitizeLogBytes(Uint8List rawData) {
  String content;
  try {
    content = utf8.decode(rawData, allowMalformed: true);
  } catch (_) {
    // 无法解码时原样返回，不阻断导出流程
    return rawData;
  }
  return Uint8List.fromList(utf8.encode(_sanitizeLogContent(content)));
}
