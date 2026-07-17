// Tests for DeviceIdentity (lib/src/services/cloud/device_identity.dart) ——
// 只覆盖纯逻辑部分：Dart Platform.operatingSystem → 契约 devicePlatform 枚举
// 字符串的映射，以及"自定义设备名优先于探测默认名"的解析优先级。
// 平台探测（Platform.isXxx / device_info_plus）依赖真实运行时环境，不在此覆盖。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:flux_down/src/services/cloud/device_identity.dart';
import 'package:flux_down/src/services/kv_store.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('platformFor', () {
    test('maps contract-covered operating systems to their wire values', () {
      expect(DeviceIdentity.platformFor('windows'), 'windows');
      expect(DeviceIdentity.platformFor('macos'), 'macos');
      expect(DeviceIdentity.platformFor('linux'), 'linux');
      expect(DeviceIdentity.platformFor('android'), 'android');
      expect(DeviceIdentity.platformFor('ios'), 'ios');
    });

    test('returns null for platforms not covered by the contract', () {
      expect(DeviceIdentity.platformFor('fuchsia'), isNull);
      expect(DeviceIdentity.platformFor('web'), isNull);
      expect(DeviceIdentity.platformFor(''), isNull);
    });
  });

  group('iosDisplayName', () {
    test('prefers the utsname machine code when present', () {
      expect(
        DeviceIdentity.iosDisplayName(machine: 'iPhone15,2', name: "Zero's iPhone"),
        'iPhone15,2',
      );
    });

    test('falls back to the user-assigned device name when machine is empty', () {
      expect(
        DeviceIdentity.iosDisplayName(machine: '  ', name: "Zero's iPhone"),
        "Zero's iPhone",
      );
    });
  });

  group('resolvedName priority', () {
    late Directory dir;
    late File file;

    setUp(() {
      dir = Directory.systemTemp.createTempSync('device_identity_test_');
      file = File('${dir.path}/settings.json');
      KvStore.instance.debugReset();
      KvStore.instance.debugInitPortable(file);
    });

    tearDown(() {
      KvStore.instance.debugReset();
      dir.deleteSync(recursive: true);
    });

    test('a user-set custom name always wins over the probed default', () async {
      await DeviceIdentity.setCustomName('我的主机');
      expect(await DeviceIdentity.resolvedName(), '我的主机');
    });

    test('blank custom names are treated as unset', () async {
      await DeviceIdentity.setCustomName('   ');
      expect(DeviceIdentity.customName(), isNull);
    });
  });
}
