///|
fn skeleton_debug_enabled() -> Bool {
  match @env.get_env_var("ASTOCK_RELEASE_TEST_CDP") {
    Some(value) =>
      match value.trim().to_owned().to_lower() {
        "" | "0" | "false" | "no" | "off" => false
        _ => true
      }
    None => false
  }
}

///|
/// Pinned Proton/CEF cold-start and memory baseline. It deliberately has no
/// commands, Workers, storage, network or application permissions.
async fn main {
  @proton.asset(
    "AStock Proton Skeleton",
    "index.html",
    width=1440,
    height=900,
    resizable=true,
  )
  .titlebar_style(@proton.TitlebarStyle::Default)
  .debug(enabled=skeleton_debug_enabled())
  .single_instance("com.astock.terminal.release-performance-skeleton")
  .run_or_abort()
}
