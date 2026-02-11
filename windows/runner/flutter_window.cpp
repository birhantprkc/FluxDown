#include "flutter_window.h"

#include <optional>
#include <string>
#include <vector>

#include "flutter/generated_plugin_registrant.h"
#include <desktop_multi_window/desktop_multi_window_plugin.h>

// Must match kCopyDataId in main.cpp.
static const ULONG_PTR kCopyDataId = 0x464C5558; // "FLUX"

// Sub-window plugin registration - only register desktop_multi_window.
// Skip window_manager (its C++ layer has a global channel variable;
// registering from multiple engines overwrites the main window's channel,
// causing crash after sub-window is destroyed).
static void RegisterPluginsForSubWindow(flutter::PluginRegistry* registry) {
  DesktopMultiWindowPluginRegisterWithRegistrar(
      registry->GetRegistrarForPlugin("DesktopMultiWindowPlugin"));
}

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());

  // Create MethodChannel for forwarding second-instance args to Dart.
  single_instance_channel_ =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          flutter_controller_->engine()->messenger(),
          "com.fluxdown/single_instance",
          &flutter::StandardMethodCodec::GetInstance());

  // Register plugin callback for sub-windows created by desktop_multi_window.
  // Sub-windows only register necessary plugins to avoid window_manager
  // global channel being overwritten.
  DesktopMultiWindowSetWindowCreatedCallback(
      [](void* controller) {
        auto* flutter_view_controller =
            reinterpret_cast<flutter::FlutterViewController*>(controller);
        RegisterPluginsForSubWindow(flutter_view_controller->engine());
      });

  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    this->Show();
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  // Handle WM_COPYDATA from a second instance before Flutter processes it.
  if (message == WM_COPYDATA) {
    auto* cds = reinterpret_cast<COPYDATASTRUCT*>(lparam);
    if (cds && cds->dwData == kCopyDataId && single_instance_channel_) {
      // Reconstruct the argument list (newline-separated UTF-8).
      std::string payload(static_cast<const char*>(cds->lpData), cds->cbData);
      flutter::EncodableList args_list;
      size_t start = 0;
      while (start < payload.size()) {
        size_t end = payload.find('\n', start);
        if (end == std::string::npos) end = payload.size();
        args_list.push_back(flutter::EncodableValue(payload.substr(start, end - start)));
        start = end + 1;
      }
      single_instance_channel_->InvokeMethod(
          "onSecondInstance",
          std::make_unique<flutter::EncodableValue>(args_list));
    }
    return 0;
  }

  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      flutter_controller_->engine()->ReloadSystemFonts();
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
