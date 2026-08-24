///|
/// Thin Windows-only helpers for operations Proton 0.2.1 does not expose as
/// public WindowHandle methods (interactive move, system menu and WM_SETICON).
/// All other window state transitions remain owned by Proton.
#borrow(path)
extern "C" fn native_apply_window_icon(path : Bytes, length : Int) -> Int = "astock_apply_window_icon"

///|
extern "C" fn native_begin_window_drag() -> Int = "astock_begin_window_drag"

///|
extern "C" fn native_show_window_system_menu() -> Int = "astock_show_window_system_menu"

///|
/// Create the Windows Job Object which owns every Worker process. The native
/// implementation enables JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE so a crashed or
/// force-closed Host cannot leave data/Agent processes behind.
extern "C" fn native_create_worker_job() -> Int = "astock_create_worker_job"

///|
extern "C" fn native_assign_worker_to_job(pid : Int) -> Int = "astock_assign_worker_to_job"

///|
extern "C" fn native_worker_job_active_processes() -> Int = "astock_worker_job_active_processes"

///|
extern "C" fn native_shutdown_worker_job(grace_ms : Int) -> Int = "astock_shutdown_worker_job"

///|
extern "C" fn native_worker_process_running(pid : Int) -> Int = "astock_worker_process_running"

///|
extern "C" fn native_terminate_worker_process(pid : Int) -> Int = "astock_terminate_worker_process"

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

///|
fn create_worker_job() -> Bool {
  native_create_worker_job() == 1
}

///|
fn assign_worker_to_job(pid : Int) -> Bool {
  native_assign_worker_to_job(pid) == 1
}

///|
fn worker_job_active_processes() -> Int {
  native_worker_job_active_processes()
}

///|
fn shutdown_worker_job(grace_ms : Int) -> Bool {
  native_shutdown_worker_job(grace_ms) == 1
}

///|
fn worker_process_running(pid : Int) -> Bool {
  native_worker_process_running(pid) == 1
}

///|
fn terminate_worker_process(pid : Int) -> Bool {
  native_terminate_worker_process(pid) == 1
}
