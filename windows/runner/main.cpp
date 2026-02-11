#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>

#include <string>

#include "flutter_window.h"
#include "utils.h"

// Unique mutex name for single-instance enforcement.
static const wchar_t kMutexName[] = L"Global\\FluxDown_SingleInstance_Mutex";
// Window class name used by Flutter runner (must match win32_window.cpp).
static const wchar_t kFlutterWindowClass[] = L"FLUTTER_RUNNER_WIN32_WINDOW";
// Window title (must match CreateCentered call below).
static const wchar_t kWindowTitle[] = L"FluxDown";
// Magic identifier for WM_COPYDATA to distinguish our messages.
static const ULONG_PTR kCopyDataId = 0x464C5558; // "FLUX" in hex

// Build a single UTF-8 string from command-line arguments, separated by '\n'.
static std::string JoinArguments(const std::vector<std::string>& args) {
  std::string result;
  for (size_t i = 0; i < args.size(); ++i) {
    if (i > 0) result += '\n';
    result += args[i];
  }
  return result;
}

// Try to find the existing FluxDown window, send it our command-line args
// via WM_COPYDATA, and bring it to the foreground.
static bool SendArgsToExistingInstance(const std::vector<std::string>& args) {
  HWND existing = ::FindWindow(kFlutterWindowClass, kWindowTitle);
  if (!existing) return false;

  // Send command-line arguments via WM_COPYDATA.
  std::string payload = JoinArguments(args);
  COPYDATASTRUCT cds = {};
  cds.dwData = kCopyDataId;
  cds.cbData = static_cast<DWORD>(payload.size());
  cds.lpData = const_cast<char*>(payload.data());
  ::SendMessage(existing, WM_COPYDATA, 0, reinterpret_cast<LPARAM>(&cds));

  // Bring existing window to foreground.
  if (::IsIconic(existing)) {
    ::ShowWindow(existing, SW_RESTORE);
  }
  ::SetForegroundWindow(existing);

  return true;
}

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  // Collect command-line arguments early (needed for both paths).
  std::vector<std::string> command_line_arguments = GetCommandLineArguments();

  // --- Single-instance check ---
  // Try to create a named mutex. If it already exists, another instance
  // is running -- forward our args to it and exit.
  HANDLE mutex = ::CreateMutex(nullptr, FALSE, kMutexName);
  if (mutex && ::GetLastError() == ERROR_ALREADY_EXISTS) {
    SendArgsToExistingInstance(command_line_arguments);
    ::CloseHandle(mutex);
    return EXIT_SUCCESS;
  }

  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

  flutter::DartProject project(L"data");

  project.set_dart_entrypoint_arguments(std::move(command_line_arguments));

  FlutterWindow window(project);
  Win32Window::Size size(1280, 720);
  if (!window.CreateCentered(kWindowTitle, size)) {
    if (mutex) ::CloseHandle(mutex);
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0)) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }

  ::CoUninitialize();
  if (mutex) ::CloseHandle(mutex);
  return EXIT_SUCCESS;
}
