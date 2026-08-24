///|
fn required_skeleton_env(name : String) -> String {
  match @env.get_env_var(name) {
    Some(value) if !value.trim().is_empty() => value.trim().to_owned()
    _ => abort("missing skeleton packaging environment variable: " + name)
  }
}

///|
/// Package the repository-pinned zero-permission Proton skeleton used only as
/// the cold-start and memory baseline for the v6 release gate.
async fn main {
  let host_executable = required_skeleton_env("ASTOCK_SKELETON_HOST_EXE")
  let desktop_root = required_skeleton_env("ASTOCK_DESKTOP_ROOT")
  let frontend_root = required_skeleton_env("ASTOCK_SKELETON_RENDERER_ROOT")
  let artifacts = required_skeleton_env("ASTOCK_SKELETON_ARTIFACTS_DIR")
  let moon_target = required_skeleton_env("ASTOCK_MOON_DESKTOP_TARGET")
  let proton_source = @build.resolve_proton_source(
    desktop_root, "backend/skeleton",
  )
  let requirements = @build.resolve_proton_runtime_requirements(proton_source)
  let helper = @build.build_cef_process(
    proton_source,
    desktop_root,
    moon_target_dir=moon_target,
  )
  let spec = @package.PackageSpec::new(
    host_executable,
    "AStock Proton Skeleton",
    "com.astock.terminal.release-performance-skeleton",
    "0.2.1",
    [App, Zip],
    artifacts,
  )
  let bundle = @bundle.ProtonBundleSpec::new(
    spec,
    helper,
    requirements,
    frontend_root,
    "0.2.1",
    "20100",
    Some(frontend_root),
  )
  let result = @bundle.build(bundle)
  for artifact in result.artifacts() {
    println("Packaged skeleton: " + artifact)
  }
}
