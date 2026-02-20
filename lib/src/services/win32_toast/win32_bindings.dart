// ignore_for_file: camel_case_types, non_constant_identifier_names, constant_identifier_names
import 'dart:ffi';

import 'package:ffi/ffi.dart';

// =============================================================================
// 基本 Win32 类型别名
// =============================================================================

typedef HWND = IntPtr;
typedef HDC = IntPtr;
typedef HBRUSH = IntPtr;
typedef HFONT = IntPtr;
typedef HBITMAP = IntPtr;
typedef HGDIOBJ = IntPtr;
typedef HINSTANCE = IntPtr;
typedef HCURSOR = IntPtr;
typedef HRGN = IntPtr;
typedef UINT_PTR = IntPtr;
typedef LONG_PTR = IntPtr;
typedef WPARAM = IntPtr;
typedef LPARAM = IntPtr;
typedef LRESULT = IntPtr;
typedef COLORREF = Uint32;

// WndProc 函数指针类型
typedef WNDPROC_Native = LRESULT Function(
  IntPtr hwnd,
  Uint32 uMsg,
  WPARAM wParam,
  LPARAM lParam,
);

// =============================================================================
// 结构体定义
// =============================================================================

/// POINT 结构体
final class POINT extends Struct {
  @Int32()
  external int x;
  @Int32()
  external int y;
}

/// RECT 结构体
final class RECT extends Struct {
  @Int32()
  external int left;
  @Int32()
  external int top;
  @Int32()
  external int right;
  @Int32()
  external int bottom;
}

/// PAINTSTRUCT 结构体（仅作备用，新实现不使用 WM_PAINT）
final class PAINTSTRUCT extends Struct {
  @IntPtr()
  external int hdc;
  @Int32()
  external int fErase;
  external RECT rcPaint;
  @Int32()
  external int fRestore;
  @Int32()
  external int fIncUpdate;
  @Array(32)
  external Array<Uint8> rgbReserved;
}

/// WNDCLASSEXW 结构体
final class WNDCLASSEXW extends Struct {
  @Uint32()
  external int cbSize;
  @Uint32()
  external int style;
  external Pointer<NativeFunction<WNDPROC_Native>> lpfnWndProc;
  @Int32()
  external int cbClsExtra;
  @Int32()
  external int cbWndExtra;
  @IntPtr()
  external int hInstance;
  @IntPtr()
  external int hIcon;
  @IntPtr()
  external int hCursor;
  @IntPtr()
  external int hbrBackground;
  external Pointer<Utf16> lpszMenuName;
  external Pointer<Utf16> lpszClassName;
  @IntPtr()
  external int hIconSm;
}

/// TRACKMOUSEEVENT 结构体（保留备用）
final class TRACKMOUSEEVENT extends Struct {
  @Uint32()
  external int cbSize;
  @Uint32()
  external int dwFlags;
  @IntPtr()
  external int hwndTrack;
  @Uint32()
  external int dwHoverTime;
}

// =============================================================================
// Win32 常量
// =============================================================================

// Window styles
const int WS_POPUP = 0x80000000;

// Extended window styles
const int WS_EX_TOPMOST = 0x00000008;
const int WS_EX_TOOLWINDOW = 0x00000080;
const int WS_EX_NOACTIVATE = 0x08000000;
const int WS_EX_LAYERED = 0x00080000;

// ShowWindow commands
const int SW_SHOWNOACTIVATE = 4;

// SetWindowPos flags
const int SWP_NOACTIVATE = 0x0010;
const int SWP_SHOWWINDOW = 0x0040;

// SetLayeredWindowAttributes flags
const int LWA_ALPHA = 0x00000002;

// GDI constants
const int TRANSPARENT = 1;
const int NULL_BRUSH = 5;

// DT_ flags for DrawTextW (user32.dll)
const int DT_LEFT = 0x00000000;
const int DT_CENTER = 0x00000001;
const int DT_RIGHT = 0x00000002;
const int DT_VCENTER = 0x00000004;
const int DT_SINGLELINE = 0x00000020;
const int DT_END_ELLIPSIS = 0x00008000;
const int DT_NOPREFIX = 0x00000800;

// SystemParametersInfoW
const int SPI_GETWORKAREA = 0x0030;

// DWM
const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
const int DWMWCP_ROUND = 2;

// Font weight
const int FW_NORMAL = 400;
const int FW_SEMIBOLD = 600;
const int FW_BOLD = 700;

// Font charset & quality
const int DEFAULT_CHARSET = 1;
const int CLEARTYPE_QUALITY = 5;
const int OUT_DEFAULT_PRECIS = 0;
const int CLIP_DEFAULT_PRECIS = 0;
const int DEFAULT_PITCH = 0;
const int FF_DONTCARE = 0;

// SRCCOPY for BitBlt
const int SRCCOPY = 0x00CC0020;

// Virtual key codes
const int VK_LBUTTON = 0x01;

// HWND insert-after values
const int HWND_TOPMOST = -1;

// =============================================================================
// DLL 句柄
// =============================================================================

final _user32 = DynamicLibrary.open('user32.dll');
final _gdi32 = DynamicLibrary.open('gdi32.dll');
final _kernel32 = DynamicLibrary.open('kernel32.dll');
final _dwmapi = DynamicLibrary.open('dwmapi.dll');

// =============================================================================
// kernel32.dll
// =============================================================================

typedef _GetModuleHandleW_Native = IntPtr Function(Pointer<Utf16> lpModuleName);
typedef _GetModuleHandleW_Dart = int Function(Pointer<Utf16> lpModuleName);

final getModuleHandleW = _kernel32
    .lookupFunction<_GetModuleHandleW_Native, _GetModuleHandleW_Dart>(
      'GetModuleHandleW',
    );

// =============================================================================
// user32.dll
// =============================================================================

// RegisterClassExW
typedef _RegisterClassExW_Native = Uint16 Function(
  Pointer<WNDCLASSEXW> lpwcx,
);
typedef _RegisterClassExW_Dart = int Function(Pointer<WNDCLASSEXW> lpwcx);
final registerClassExW = _user32
    .lookupFunction<_RegisterClassExW_Native, _RegisterClassExW_Dart>(
      'RegisterClassExW',
    );

// CreateWindowExW
typedef _CreateWindowExW_Native =
    IntPtr Function(
      Uint32 dwExStyle,
      Pointer<Utf16> lpClassName,
      Pointer<Utf16> lpWindowName,
      Uint32 dwStyle,
      Int32 x,
      Int32 y,
      Int32 nWidth,
      Int32 nHeight,
      IntPtr hWndParent,
      IntPtr hMenu,
      IntPtr hInstance,
      Pointer<Void> lpParam,
    );
typedef _CreateWindowExW_Dart =
    int Function(
      int dwExStyle,
      Pointer<Utf16> lpClassName,
      Pointer<Utf16> lpWindowName,
      int dwStyle,
      int x,
      int y,
      int nWidth,
      int nHeight,
      int hWndParent,
      int hMenu,
      int hInstance,
      Pointer<Void> lpParam,
    );
final createWindowExW = _user32
    .lookupFunction<_CreateWindowExW_Native, _CreateWindowExW_Dart>(
      'CreateWindowExW',
    );

// DestroyWindow
typedef _DestroyWindow_Native = Int32 Function(IntPtr hWnd);
typedef _DestroyWindow_Dart = int Function(int hWnd);
final destroyWindow = _user32
    .lookupFunction<_DestroyWindow_Native, _DestroyWindow_Dart>(
      'DestroyWindow',
    );

// ShowWindow
typedef _ShowWindow_Native = Int32 Function(IntPtr hWnd, Int32 nCmdShow);
typedef _ShowWindow_Dart = int Function(int hWnd, int nCmdShow);
final showWindow = _user32
    .lookupFunction<_ShowWindow_Native, _ShowWindow_Dart>('ShowWindow');

// DefWindowProcW — 直接获取原生函数指针，用作 WndProc（绕过 Dart isolate）
final defWindowProcWPtr = _user32
    .lookup<NativeFunction<WNDPROC_Native>>('DefWindowProcW');

// GetSystemMetrics
typedef _GetSystemMetrics_Native = Int32 Function(Int32 nIndex);
typedef _GetSystemMetrics_Dart = int Function(int nIndex);
final getSystemMetrics = _user32
    .lookupFunction<_GetSystemMetrics_Native, _GetSystemMetrics_Dart>(
      'GetSystemMetrics',
    );

// SystemParametersInfoW
typedef _SystemParametersInfoW_Native =
    Int32 Function(
      Uint32 uiAction,
      Uint32 uiParam,
      Pointer<Void> pvParam,
      Uint32 fWinIni,
    );
typedef _SystemParametersInfoW_Dart =
    int Function(
      int uiAction,
      int uiParam,
      Pointer<Void> pvParam,
      int fWinIni,
    );
final systemParametersInfoW = _user32
    .lookupFunction<
      _SystemParametersInfoW_Native,
      _SystemParametersInfoW_Dart
    >('SystemParametersInfoW');

// SetWindowPos
typedef _SetWindowPos_Native =
    Int32 Function(
      IntPtr hWnd,
      IntPtr hWndInsertAfter,
      Int32 X,
      Int32 Y,
      Int32 cx,
      Int32 cy,
      Uint32 uFlags,
    );
typedef _SetWindowPos_Dart =
    int Function(
      int hWnd,
      int hWndInsertAfter,
      int X,
      int Y,
      int cx,
      int cy,
      int uFlags,
    );
final setWindowPos = _user32
    .lookupFunction<_SetWindowPos_Native, _SetWindowPos_Dart>('SetWindowPos');

// SetLayeredWindowAttributes
typedef _SetLayeredWindowAttributes_Native =
    Int32 Function(
      IntPtr hwnd,
      COLORREF crKey,
      Uint8 bAlpha,
      Uint32 dwFlags,
    );
typedef _SetLayeredWindowAttributes_Dart =
    int Function(int hwnd, int crKey, int bAlpha, int dwFlags);
final setLayeredWindowAttributes = _user32
    .lookupFunction<
      _SetLayeredWindowAttributes_Native,
      _SetLayeredWindowAttributes_Dart
    >('SetLayeredWindowAttributes');

// GetCursorPos
typedef _GetCursorPos_Native = Int32 Function(Pointer<POINT> lpPoint);
typedef _GetCursorPos_Dart = int Function(Pointer<POINT> lpPoint);
final getCursorPos = _user32
    .lookupFunction<_GetCursorPos_Native, _GetCursorPos_Dart>('GetCursorPos');

// ScreenToClient
typedef _ScreenToClient_Native = Int32 Function(
  IntPtr hWnd,
  Pointer<POINT> lpPoint,
);
typedef _ScreenToClient_Dart = int Function(int hWnd, Pointer<POINT> lpPoint);
final screenToClient = _user32
    .lookupFunction<_ScreenToClient_Native, _ScreenToClient_Dart>(
      'ScreenToClient',
    );

// SetWindowRgn
typedef _SetWindowRgn_Native = Int32 Function(
  IntPtr hWnd,
  HRGN hRgn,
  Int32 bRedraw,
);
typedef _SetWindowRgn_Dart = int Function(int hWnd, int hRgn, int bRedraw);
final setWindowRgn = _user32
    .lookupFunction<_SetWindowRgn_Native, _SetWindowRgn_Dart>('SetWindowRgn');

// LoadCursorW
typedef _LoadCursorW_Native = HCURSOR Function(
  HINSTANCE hInstance,
  Pointer<Utf16> lpCursorName,
);
typedef _LoadCursorW_Dart = int Function(
  int hInstance,
  Pointer<Utf16> lpCursorName,
);
final loadCursorW = _user32
    .lookupFunction<_LoadCursorW_Native, _LoadCursorW_Dart>('LoadCursorW');

// GetDpiForWindow
typedef _GetDpiForWindow_Native = Uint32 Function(IntPtr hwnd);
typedef _GetDpiForWindow_Dart = int Function(int hwnd);
final getDpiForWindow = _user32
    .lookupFunction<_GetDpiForWindow_Native, _GetDpiForWindow_Dart>(
      'GetDpiForWindow',
    );

// GetClientRect
typedef _GetClientRect_Native = Int32 Function(
  IntPtr hWnd,
  Pointer<RECT> lpRect,
);
typedef _GetClientRect_Dart = int Function(int hWnd, Pointer<RECT> lpRect);
final getClientRect = _user32
    .lookupFunction<_GetClientRect_Native, _GetClientRect_Dart>(
      'GetClientRect',
    );

// GetDC — 获取窗口设备上下文（用于在 WM_PAINT 之外绘制）
typedef _GetDC_Native = HDC Function(IntPtr hWnd);
typedef _GetDC_Dart = int Function(int hWnd);
final getDC = _user32.lookupFunction<_GetDC_Native, _GetDC_Dart>('GetDC');

// ReleaseDC — 释放 GetDC 获取的 DC
typedef _ReleaseDC_Native = Int32 Function(IntPtr hWnd, HDC hDC);
typedef _ReleaseDC_Dart = int Function(int hWnd, int hDC);
final releaseDC = _user32
    .lookupFunction<_ReleaseDC_Native, _ReleaseDC_Dart>('ReleaseDC');

// ValidateRect — 将区域标记为有效，阻止 WM_PAINT 派发
typedef _ValidateRect_Native = Int32 Function(
  IntPtr hWnd,
  Pointer<RECT> lpRect,
);
typedef _ValidateRect_Dart = int Function(int hWnd, Pointer<RECT> lpRect);
final validateRect = _user32
    .lookupFunction<_ValidateRect_Native, _ValidateRect_Dart>('ValidateRect');

// GetAsyncKeyState — 查询键/鼠标按钮异步状态（返回 SHORT）
typedef _GetAsyncKeyState_Native = Int16 Function(Int32 vKey);
typedef _GetAsyncKeyState_Dart = int Function(int vKey);
final getAsyncKeyState = _user32
    .lookupFunction<_GetAsyncKeyState_Native, _GetAsyncKeyState_Dart>(
      'GetAsyncKeyState',
    );

// FillRect — 用画刷填充矩形
typedef _FillRect_Native = Int32 Function(
  HDC hDC,
  Pointer<RECT> lprc,
  HBRUSH hbr,
);
typedef _FillRect_Dart = int Function(int hDC, Pointer<RECT> lprc, int hbr);
final fillRect = _user32
    .lookupFunction<_FillRect_Native, _FillRect_Dart>('FillRect');

// DrawTextW — 在矩形内绘制格式化文本（user32.dll，非 gdi32.dll）
typedef _DrawTextW_Native =
    Int32 Function(
      HDC hdc,
      Pointer<Utf16> lpchText,
      Int32 cchText,
      Pointer<RECT> lprc,
      Uint32 format,
    );
typedef _DrawTextW_Dart =
    int Function(
      int hdc,
      Pointer<Utf16> lpchText,
      int cchText,
      Pointer<RECT> lprc,
      int format,
    );
final drawTextW = _user32
    .lookupFunction<_DrawTextW_Native, _DrawTextW_Dart>('DrawTextW');

// =============================================================================
// gdi32.dll
// =============================================================================

// CreateSolidBrush
typedef _CreateSolidBrush_Native = HBRUSH Function(COLORREF color);
typedef _CreateSolidBrush_Dart = int Function(int color);
final createSolidBrush = _gdi32
    .lookupFunction<_CreateSolidBrush_Native, _CreateSolidBrush_Dart>(
      'CreateSolidBrush',
    );

// DeleteObject
typedef _DeleteObject_Native = Int32 Function(HGDIOBJ ho);
typedef _DeleteObject_Dart = int Function(int ho);
final deleteObject = _gdi32
    .lookupFunction<_DeleteObject_Native, _DeleteObject_Dart>('DeleteObject');

// CreateFontW
typedef _CreateFontW_Native =
    HFONT Function(
      Int32 cHeight,
      Int32 cWidth,
      Int32 cEscapement,
      Int32 cOrientation,
      Int32 cWeight,
      Uint32 bItalic,
      Uint32 bUnderline,
      Uint32 bStrikeOut,
      Uint32 iCharSet,
      Uint32 iOutPrecision,
      Uint32 iClipPrecision,
      Uint32 iQuality,
      Uint32 iPitchAndFamily,
      Pointer<Utf16> pszFaceName,
    );
typedef _CreateFontW_Dart =
    int Function(
      int cHeight,
      int cWidth,
      int cEscapement,
      int cOrientation,
      int cWeight,
      int bItalic,
      int bUnderline,
      int bStrikeOut,
      int iCharSet,
      int iOutPrecision,
      int iClipPrecision,
      int iQuality,
      int iPitchAndFamily,
      Pointer<Utf16> pszFaceName,
    );
final createFontW = _gdi32
    .lookupFunction<_CreateFontW_Native, _CreateFontW_Dart>('CreateFontW');

// SelectObject
typedef _SelectObject_Native = HGDIOBJ Function(HDC hdc, HGDIOBJ h);
typedef _SelectObject_Dart = int Function(int hdc, int h);
final selectObject = _gdi32
    .lookupFunction<_SelectObject_Native, _SelectObject_Dart>('SelectObject');

// SetBkMode
typedef _SetBkMode_Native = Int32 Function(HDC hdc, Int32 mode);
typedef _SetBkMode_Dart = int Function(int hdc, int mode);
final setBkMode = _gdi32
    .lookupFunction<_SetBkMode_Native, _SetBkMode_Dart>('SetBkMode');

// SetTextColor
typedef _SetTextColor_Native = COLORREF Function(HDC hdc, COLORREF color);
typedef _SetTextColor_Dart = int Function(int hdc, int color);
final setTextColor = _gdi32
    .lookupFunction<_SetTextColor_Native, _SetTextColor_Dart>('SetTextColor');

// GetStockObject
typedef _GetStockObject_Native = HGDIOBJ Function(Int32 i);
typedef _GetStockObject_Dart = int Function(int i);
final getStockObject = _gdi32
    .lookupFunction<_GetStockObject_Native, _GetStockObject_Dart>(
      'GetStockObject',
    );

// CreateRoundRectRgn
typedef _CreateRoundRectRgn_Native =
    HRGN Function(
      Int32 x1,
      Int32 y1,
      Int32 x2,
      Int32 y2,
      Int32 w,
      Int32 h,
    );
typedef _CreateRoundRectRgn_Dart =
    int Function(int x1, int y1, int x2, int y2, int w, int h);
final createRoundRectRgn = _gdi32
    .lookupFunction<_CreateRoundRectRgn_Native, _CreateRoundRectRgn_Dart>(
      'CreateRoundRectRgn',
    );

// CreateCompatibleDC
typedef _CreateCompatibleDC_Native = HDC Function(HDC hdc);
typedef _CreateCompatibleDC_Dart = int Function(int hdc);
final createCompatibleDC = _gdi32
    .lookupFunction<_CreateCompatibleDC_Native, _CreateCompatibleDC_Dart>(
      'CreateCompatibleDC',
    );

// CreateCompatibleBitmap
typedef _CreateCompatibleBitmap_Native = HBITMAP Function(
  HDC hdc,
  Int32 cx,
  Int32 cy,
);
typedef _CreateCompatibleBitmap_Dart = int Function(int hdc, int cx, int cy);
final createCompatibleBitmap = _gdi32
    .lookupFunction<
      _CreateCompatibleBitmap_Native,
      _CreateCompatibleBitmap_Dart
    >('CreateCompatibleBitmap');

// BitBlt
typedef _BitBlt_Native =
    Int32 Function(
      HDC hdc,
      Int32 x,
      Int32 y,
      Int32 cx,
      Int32 cy,
      HDC hdcSrc,
      Int32 x1,
      Int32 y1,
      Uint32 rop,
    );
typedef _BitBlt_Dart =
    int Function(
      int hdc,
      int x,
      int y,
      int cx,
      int cy,
      int hdcSrc,
      int x1,
      int y1,
      int rop,
    );
final bitBlt = _gdi32.lookupFunction<_BitBlt_Native, _BitBlt_Dart>('BitBlt');

// DeleteDC
typedef _DeleteDC_Native = Int32 Function(HDC hdc);
typedef _DeleteDC_Dart = int Function(int hdc);
final deleteDC = _gdi32
    .lookupFunction<_DeleteDC_Native, _DeleteDC_Dart>('DeleteDC');

// =============================================================================
// dwmapi.dll
// =============================================================================

typedef _DwmSetWindowAttribute_Native =
    Int32 Function(
      IntPtr hwnd,
      Uint32 dwAttribute,
      Pointer<Void> pvAttribute,
      Uint32 cbAttribute,
    );
typedef _DwmSetWindowAttribute_Dart =
    int Function(
      int hwnd,
      int dwAttribute,
      Pointer<Void> pvAttribute,
      int cbAttribute,
    );
final dwmSetWindowAttribute = _dwmapi
    .lookupFunction<
      _DwmSetWindowAttribute_Native,
      _DwmSetWindowAttribute_Dart
    >('DwmSetWindowAttribute');

// =============================================================================
// 工具函数
// =============================================================================

/// 将 Dart Color int（0xAARRGGBB）转换为 COLORREF（0x00BBGGRR）
int colorToCOLORREF(int argb) {
  final r = (argb >> 16) & 0xFF;
  final g = (argb >> 8) & 0xFF;
  final b = argb & 0xFF;
  return (b << 16) | (g << 8) | r;
}
