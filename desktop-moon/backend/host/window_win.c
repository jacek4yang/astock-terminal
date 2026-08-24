#include <stdint.h>

#include "moonbit.h"

#if defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

typedef struct astock_window_lookup {
  DWORD process_id;
  HWND window;
} astock_window_lookup_t;

static BOOL CALLBACK astock_find_window_callback(HWND window, LPARAM data) {
  astock_window_lookup_t *lookup = (astock_window_lookup_t *)data;
  DWORD process_id = 0;
  GetWindowThreadProcessId(window, &process_id);
  if (process_id != lookup->process_id || GetWindow(window, GW_OWNER) != NULL ||
      !IsWindowVisible(window)) {
    return TRUE;
  }
  wchar_t class_name[96] = {0};
  GetClassNameW(window, class_name,
                (int)(sizeof(class_name) / sizeof(class_name[0])));
  if (wcsstr(class_name, L"Proton") == NULL) {
    return TRUE;
  }
  lookup->window = window;
  return FALSE;
}

static HWND astock_main_window(void) {
  astock_window_lookup_t lookup = {GetCurrentProcessId(), NULL};
  EnumWindows(astock_find_window_callback, (LPARAM)&lookup);
  return lookup.window;
}

static wchar_t *astock_utf8_path(moonbit_bytes_t path, int32_t length) {
  if (path == NULL || length <= 0) {
    return NULL;
  }
  int wide_length = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                        (const char *)path, length, NULL, 0);
  if (wide_length <= 0) {
    return NULL;
  }
  wchar_t *wide = (wchar_t *)HeapAlloc(
      GetProcessHeap(), HEAP_ZERO_MEMORY,
      ((size_t)wide_length + 1) * sizeof(wchar_t));
  if (wide == NULL) {
    return NULL;
  }
  if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, (const char *)path,
                          length, wide, wide_length) != wide_length) {
    HeapFree(GetProcessHeap(), 0, wide);
    return NULL;
  }
  return wide;
}

static HICON astock_load_icon(const wchar_t *path, int width, int height) {
  HICON icon = NULL;
  if (path != NULL && path[0] != L'\0') {
    icon = (HICON)LoadImageW(NULL, path, IMAGE_ICON, width, height,
                            LR_LOADFROMFILE | LR_DEFAULTCOLOR);
  }
  if (icon == NULL) {
    icon = (HICON)LoadImageW(GetModuleHandleW(NULL), MAKEINTRESOURCEW(1),
                            IMAGE_ICON, width, height, LR_DEFAULTCOLOR);
  }
  return icon;
}

MOONBIT_FFI_EXPORT int32_t astock_apply_window_icon(moonbit_bytes_t path,
                                                     int32_t length) {
  HWND window = astock_main_window();
  if (window == NULL) {
    return 0;
  }
  wchar_t *wide_path = astock_utf8_path(path, length);
  UINT dpi = GetDpiForWindow(window);
  if (dpi == 0) {
    dpi = USER_DEFAULT_SCREEN_DPI;
  }
  int large_size = GetSystemMetricsForDpi(SM_CXICON, dpi);
  int small_size = GetSystemMetricsForDpi(SM_CXSMICON, dpi);
  HICON large = astock_load_icon(wide_path, large_size, large_size);
  HICON small = astock_load_icon(wide_path, small_size, small_size);
  if (wide_path != NULL) {
    HeapFree(GetProcessHeap(), 0, wide_path);
  }
  if (large == NULL && small == NULL) {
    return 0;
  }
  if (large != NULL) {
    SendMessageW(window, WM_SETICON, ICON_BIG, (LPARAM)large);
  }
  if (small != NULL) {
    SendMessageW(window, WM_SETICON, ICON_SMALL, (LPARAM)small);
    SendMessageW(window, WM_SETICON, ICON_SMALL2, (LPARAM)small);
  }
  RedrawWindow(window, NULL, NULL,
               RDW_FRAME | RDW_INVALIDATE | RDW_UPDATENOW);
  return 1;
}

MOONBIT_FFI_EXPORT int32_t astock_begin_window_drag(void) {
  HWND window = astock_main_window();
  if (window == NULL || IsZoomed(window)) {
    return 0;
  }
  ReleaseCapture();
  SendMessageW(window, WM_NCLBUTTONDOWN, HTCAPTION, 0);
  return 1;
}

MOONBIT_FFI_EXPORT int32_t astock_show_window_system_menu(void) {
  HWND window = astock_main_window();
  if (window == NULL) {
    return 0;
  }
  HMENU menu = GetSystemMenu(window, FALSE);
  POINT cursor;
  if (menu == NULL || !GetCursorPos(&cursor)) {
    return 0;
  }
  SetForegroundWindow(window);
  UINT command = TrackPopupMenu(
      menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_TOPALIGN | TPM_LEFTALIGN,
      cursor.x, cursor.y, 0, window, NULL);
  if (command != 0) {
    PostMessageW(window, WM_SYSCOMMAND, command, 0);
  }
  return 1;
}

#else

MOONBIT_FFI_EXPORT int32_t astock_apply_window_icon(moonbit_bytes_t path,
                                                     int32_t length) {
  (void)path;
  (void)length;
  return 0;
}

MOONBIT_FFI_EXPORT int32_t astock_begin_window_drag(void) { return 0; }

MOONBIT_FFI_EXPORT int32_t astock_show_window_system_menu(void) { return 0; }

#endif
