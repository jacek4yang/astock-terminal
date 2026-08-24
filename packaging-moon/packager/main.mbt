///| Packaging adapter that consumes the already-built Host executable. This
/// deliberately bypasses `proton_cli package`, whose 0.2.1 implementation
/// creates a new project-local `_build` tree.
fn required_env(name : String) -> String {
  match @env.get_env_var(name) {
    Some(value) if !value.trim().is_empty() => value.trim().to_owned()
    _ => abort("missing packaging environment variable: " + name)
  }
}

///|
async fn main {
  let host_executable = required_env("ASTOCK_HOST_EXE")
  let desktop_root = required_env("ASTOCK_DESKTOP_ROOT")
  let frontend_dist = required_env("ASTOCK_RENDERER_DIST")
  let worker_resources = required_env("ASTOCK_PACKAGE_STAGE") + "\\workers"
  let artifacts = required_env("ASTOCK_ARTIFACTS_DIR")
  let moon_target = required_env("ASTOCK_MOON_DESKTOP_TARGET")
  let app_icon = desktop_root + "\\..\\src-tauri\\icons\\icon.ico"

  let proton_source = @build.resolve_proton_source(desktop_root, "backend/app")
  let requirements = @build.resolve_proton_runtime_requirements(proton_source)
  let helper = @build.build_cef_process(
    proton_source,
    desktop_root,
    moon_target_dir=moon_target,
  )
  let spec = @package.PackageSpec::new(
    host_executable,
    "AStock Terminal",
    "com.astock.terminal",
    "5.1.0",
    [App, Zip, Nsis],
    artifacts,
    icons=[app_icon],
  )
  let bundle = @bundle.ProtonBundleSpec::new(
    spec,
    helper,
    requirements,
    desktop_root,
    "5.1.0",
    "50100",
    Some(frontend_dist),
    resources=[worker_resources],
  )
  let result = @bundle.build(bundle)
  for artifact in result.artifacts() {
    println("Packaged: " + artifact)
  }
}
