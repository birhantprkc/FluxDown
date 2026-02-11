import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:launch_at_startup/launch_at_startup.dart';
import 'package:rinf/rinf.dart';

import '../bindings/bindings.dart';
import '../services/log_service.dart';

/// 下载引擎相关配置（持久化在 Rust SQLite 中）
class SettingsProvider extends ChangeNotifier {
  /// 全局单例引用，供 WindowListener 等无 context 场景读取设置
  static SettingsProvider? globalInstance;

  String _defaultSaveDir = _platformDefaultSaveDir();
  int _defaultSegments = 0; // 0 = 自动（由 Rust segment_advisor 动态计算）
  int _maxConcurrentTasks = 5;
  int _speedLimitBytes = 0; // 0 = 无限制
  bool _autoResumeOnStart = false;
  bool _closeToTray = true; // 默认关闭到托盘
  bool _autoStartup = false; // 默认不开机启动
  bool _autoCheckUpdate = true; // 默认启动时自动检查更新

  // BT 设置
  bool _btEnableDht = true; // DHT 分布式哈希表
  bool _btEnableUpnp = true; // UPnP 端口映射
  int _btPortStart = 6881; // 监听端口起始
  int _btPortEnd = 6891; // 监听端口结束
  String _btCustomTrackers = ''; // 用户自定义 Tracker 列表（换行分隔）

  /// 配置是否已从 Rust 端加载完成
  bool _loaded = false;

  StreamSubscription<RustSignalPack<ConfigLoaded>>? _configSub;

  SettingsProvider() {
    logInfo('Settings', 'constructor, setting globalInstance');
    globalInstance = this;
    _startListening();
    _syncAutoStartupState();
  }

  @override
  void dispose() {
    logInfo('Settings', 'dispose');
    _configSub?.cancel();
    super.dispose();
  }

  // ---------------------------------------------------------------------------
  // Getters
  // ---------------------------------------------------------------------------

  bool get loaded => _loaded;
  String get defaultSaveDir => _defaultSaveDir;
  int get defaultSegments => _defaultSegments;
  int get maxConcurrentTasks => _maxConcurrentTasks;
  int get speedLimitBytes => _speedLimitBytes;
  bool get autoResumeOnStart => _autoResumeOnStart;
  bool get closeToTray => _closeToTray;
  bool get autoStartup => _autoStartup;
  bool get autoCheckUpdate => _autoCheckUpdate;

  // BT 设置 Getters
  bool get btEnableDht => _btEnableDht;
  bool get btEnableUpnp => _btEnableUpnp;
  int get btPortStart => _btPortStart;
  int get btPortEnd => _btPortEnd;
  String get btCustomTrackers => _btCustomTrackers;

  // ---------------------------------------------------------------------------
  // Setters — 修改值 + 通知 Rust 持久化
  // ---------------------------------------------------------------------------

  void setDefaultSaveDir(String value) {
    if (_defaultSaveDir == value) return;
    _defaultSaveDir = value;
    notifyListeners();
    _saveToRust('default_save_dir', value);
  }

  void setDefaultSegments(int value) {
    if (_defaultSegments == value) return;
    _defaultSegments = value;
    notifyListeners();
    _saveToRust('default_segments', value.toString());
  }

  void setMaxConcurrentTasks(int value) {
    if (_maxConcurrentTasks == value) return;
    _maxConcurrentTasks = value;
    notifyListeners();
    _saveToRust('max_concurrent_tasks', value.toString());
  }

  void setSpeedLimitBytes(int value) {
    if (_speedLimitBytes == value) return;
    _speedLimitBytes = value;
    notifyListeners();
    _saveToRust('speed_limit_bytes', value.toString());
  }

  void setAutoResumeOnStart(bool value) {
    if (_autoResumeOnStart == value) return;
    _autoResumeOnStart = value;
    notifyListeners();
    _saveToRust('auto_resume_on_start', value.toString());
  }

  void setCloseToTray(bool value) {
    if (_closeToTray == value) return;
    _closeToTray = value;
    notifyListeners();
    _saveToRust('close_to_tray', value.toString());
  }

  void setAutoCheckUpdate(bool value) {
    if (_autoCheckUpdate == value) return;
    _autoCheckUpdate = value;
    notifyListeners();
    _saveToRust('auto_check_update', value.toString());
  }

  // BT 设置 Setters

  void setBtEnableDht(bool value) {
    if (_btEnableDht == value) return;
    _btEnableDht = value;
    notifyListeners();
    _saveToRust('bt_enable_dht', value.toString());
  }

  void setBtEnableUpnp(bool value) {
    if (_btEnableUpnp == value) return;
    _btEnableUpnp = value;
    notifyListeners();
    _saveToRust('bt_enable_upnp', value.toString());
  }

  void setBtPortStart(int value) {
    if (_btPortStart == value) return;
    _btPortStart = value;
    notifyListeners();
    _saveToRust('bt_port_start', value.toString());
  }

  void setBtPortEnd(int value) {
    if (_btPortEnd == value) return;
    _btPortEnd = value;
    notifyListeners();
    _saveToRust('bt_port_end', value.toString());
  }

  void setBtCustomTrackers(String value) {
    if (_btCustomTrackers == value) return;
    _btCustomTrackers = value;
    notifyListeners();
    _saveToRust('bt_custom_trackers', value);
  }

  /// 设置开机自启动，返回是否成功。
  /// 操作后通过 [launchAtStartup.isEnabled] 验证注册表实际状态，
  /// 若与预期不符则回滚 UI 状态。
  Future<bool> setAutoStartup(bool value) async {
    if (_autoStartup == value) return true;

    // 先乐观更新 UI
    _autoStartup = value;
    notifyListeners();

    try {
      if (value) {
        await launchAtStartup.enable();
      } else {
        await launchAtStartup.disable();
      }

      // 验证实际状态
      final actual = await launchAtStartup.isEnabled();
      if (actual == value) {
        _saveToRust('auto_startup', value.toString());
        return true;
      }

      // 验证失败 — 回滚
      _autoStartup = !value;
      notifyListeners();
      return false;
    } catch (_) {
      // 异常 — 回滚
      _autoStartup = !value;
      notifyListeners();
      return false;
    }
  }

  // ---------------------------------------------------------------------------
  // 请求 Rust 端加载配置
  // ---------------------------------------------------------------------------

  void requestConfig() {
    const RequestConfig().sendSignalToRust();
  }

  // ---------------------------------------------------------------------------
  // 内部
  // ---------------------------------------------------------------------------

  void _startListening() {
    _configSub = ConfigLoaded.rustSignalStream.listen(_onConfigLoaded);
  }

  void _onConfigLoaded(RustSignalPack<ConfigLoaded> pack) {
    final entries = pack.message.entries;
    logInfo('Settings', '_onConfigLoaded: ${entries.length} entries');
    for (final entry in entries) {
      logInfo('Settings', '  config: ${entry.key}=${entry.value}');
      switch (entry.key) {
        case 'default_save_dir':
          _defaultSaveDir = entry.value;
        case 'default_segments':
          _defaultSegments = int.tryParse(entry.value) ?? 0;
        case 'max_concurrent_tasks':
          _maxConcurrentTasks = int.tryParse(entry.value) ?? 5;
        case 'speed_limit_bytes':
          _speedLimitBytes = int.tryParse(entry.value) ?? 0;
        case 'auto_resume_on_start':
          _autoResumeOnStart = entry.value == 'true';
        case 'close_to_tray':
          _closeToTray = entry.value == 'true';
        case 'auto_startup':
          _autoStartup = entry.value == 'true';
        case 'auto_check_update':
          _autoCheckUpdate = entry.value == 'true';
        case 'bt_enable_dht':
          _btEnableDht = entry.value == 'true';
        case 'bt_enable_upnp':
          _btEnableUpnp = entry.value == 'true';
        case 'bt_port_start':
          _btPortStart = int.tryParse(entry.value) ?? 6881;
        case 'bt_port_end':
          _btPortEnd = int.tryParse(entry.value) ?? 6891;
        case 'bt_custom_trackers':
          _btCustomTrackers = entry.value;
      }
    }
    _loaded = true;
    notifyListeners();
  }

  void _saveToRust(String key, String value) {
    SaveConfig(key: key, value: value).sendSignalToRust();
  }

  /// 启动时同步开机启动状态（从系统注册表读取实际状态）
  Future<void> _syncAutoStartupState() async {
    _autoStartup = await launchAtStartup.isEnabled();
  }

  /// 平台默认下载目录
  static String _platformDefaultSaveDir() {
    final home =
        Platform.environment['USERPROFILE'] ??
        Platform.environment['HOME'] ??
        '.';
    return '$home${Platform.pathSeparator}Downloads';
  }
}
