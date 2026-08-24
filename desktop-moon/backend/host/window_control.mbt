///| Thin Windows-only helpers for operations Proton 0.2.1 does not expose as
/// public WindowHandle methods (interactive move, system menu and WM_SETICON).
/// All other window state transitions remain owned by Proton.
#borrow(path)
extern "C" fn native_apply_window_icon(path : Bytes, length : Int) -> Int =
  "astock_apply_window_icon"

///|
extern "C" fn native_begin_window_drag() -> Int = "astock_begin_window_drag"

///|
extern "C" fn native_show_window_system_menu() -> Int =
  "astock_show_window_system_menu"

///|
fn apply_window_icon(path : String) -> Bool {
  let bytes = @utf8.encode(path)
  native_apply_window_icon(bytes, bytes.length()) == 1
}

///|
fn begin_window_drag() -> Bool {
  native_begin_window_drag() == 1
}

///|
fn show_window_system_menu() -> Bool {
  native_show_window_system_menu() == 1
}
