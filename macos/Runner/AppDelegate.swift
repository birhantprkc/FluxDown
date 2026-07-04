import Cocoa
import FlutterMacOS
import UserNotifications

@main
class AppDelegate: FlutterAppDelegate, UNUserNotificationCenterDelegate {
  override func applicationDidFinishLaunching(_ notification: Notification) {
    UNUserNotificationCenter.current().delegate = self
    super.applicationDidFinishLaunching(notification)
  }

  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return false
  }

  /// 点击 Dock 图标时恢复主窗口（详见 restoreMainWindow）。
  override func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
    restoreMainWindow()
    return false
  }

  /// 可靠地把主窗口从「关闭到托盘」(orderOut) 或最小化恢复并置于前台。
  ///
  /// 供 Dock 点击 (applicationShouldHandleReopen) 与托盘/悬浮球点击
  /// (MethodChannel `com.fluxdown/window` → restore) 共用。
  ///
  /// 不走 window_manager 的 show()/focus()：其 focus() 使用
  /// NSApp.activate(ignoringOtherApps: false)，在用户已切到别的 App 后
  /// 点击托盘时，macOS 13+ 常常不把本 App 带到前台，导致窗口 orderFront
  /// 后仍停留在后台不可见，用户以为「打不开」只能退出重开。这里统一用
  /// ignoringOtherApps: true 强制前台。
  /// 注意：不遍历 NSApp.windows —— 悬浮球 FloatingBallPanel 也在其中，
  /// 不能被激活聚焦。
  func restoreMainWindow() {
    guard let window = mainFlutterWindow else { return }
    if window.isMiniaturized {
      window.deminiaturize(nil)
    }
    if !window.isVisible {
      window.setIsVisible(true)
    }
    window.makeKeyAndOrderFront(self)
    NSApp.activate(ignoringOtherApps: true)
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}
