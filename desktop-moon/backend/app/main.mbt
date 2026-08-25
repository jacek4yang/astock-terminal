///|
fn environment_flag(name : String) -> Bool {
  match @env.get_env_var(name) {
    Some(value) =>
      match value.trim().to_owned().to_lower() {
        "" | "0" | "false" | "no" | "off" => false
        _ => true
      }
    None => false
  }
}

///|
/// Windows-first Proton/CEF Host. The Host owns only desktop orchestration,
/// permissions, Worker supervision, IPC routing and diagnostics.
async fn main {
  @async.with_task_group() <| group => {
    let backend = @host.Backend::start() catch {
      error => abort("AStock Worker startup failed: " + error.to_string())
    }
    defer backend.close()
    group.spawn_bg(no_wait=true, allow_failure=true, () => backend.supervise())
    let application = @proton.asset(
      "AStock Terminal",
      "index.html",
      width=1440,
      height=900,
      resizable=true,
    )
    let application = if environment_flag("PROTON_HEADLESS") {
      // Proton rejects overlay titlebars in off-screen mode. This path exists
      // only for packaged release measurement; normal desktop windows retain
      // the native overlay and its drag/maximize behavior.
      application.titlebar_style(@proton.TitlebarStyle::Default)
    } else {
      application.titlebar_style(@proton.TitlebarStyle::Overlay)
    }
    application
    // CEF CDP is deny-by-default. The production shortcut never sets this
    // variable; release automation opts in on a random loopback-only port.
    .debug(enabled=environment_flag("ASTOCK_RELEASE_TEST_CDP"))
    .single_instance("com.astock.terminal")
    .window_lifecycle(
      on_ready=fn(context) {
        backend.attach_window(context.handle(), context.events())
      },
      on_close=fn(_state) { backend.detach_window() },
    )
    .commands(fn(registrar) raise { backend.register_commands(registrar) })
    .permission(
      @proton.PermissionGrant::new(
        "main",
        @proton.PermissionOrigin::Entry,
        "app",
      ),
    )
    .run_or_abort()
  }
}
