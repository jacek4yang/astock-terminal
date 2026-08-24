///|
/// Windows-first Proton/CEF Host. The Host owns only desktop orchestration,
/// permissions, Worker supervision, IPC routing and diagnostics.
async fn main {
  let backend = @host.Backend::start() catch {
    error => abort("AStock Worker startup failed: " + error.to_string())
  }
  defer backend.close()
  @proton.asset(
    "AStock Terminal",
    "index.html",
    width=1440,
    height=900,
    resizable=true,
  )
  .titlebar_style(@proton.TitlebarStyle::Overlay)
  .single_instance("com.astock.terminal")
  .window_lifecycle(
    on_ready=fn(context) { backend.attach_window(context.handle()) },
    on_close=fn(_state) { backend.detach_window() },
  )
  .commands(fn(registrar) raise { backend.register_commands(registrar) })
  .permission(
    @proton.PermissionGrant::new("main", @proton.PermissionOrigin::Entry, "app"),
  )
  .run_or_abort()
}
