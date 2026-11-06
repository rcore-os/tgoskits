use super::*;

pub(super) fn runtime_name(runtime: KtestRuntime) -> &'static str {
    match runtime {
        KtestRuntime::Arceos => "arceos",
        KtestRuntime::Starry => "starry",
        KtestRuntime::Axvisor => "axvisor",
        KtestRuntime::Board => "board",
    }
}


pub(super) fn maybe_postprocess_starry_artifact(
    ctx: KtestBuildContext<'_>,
    cargo: &Cargo,
    output: &ostool::build::CargoBuildOutput,
) -> anyhow::Result<()> {
    if ctx.runtime != KtestRuntime::Starry {
        return Ok(());
    }
    let request = ctx
        .config
        .starry_request(ctx.package, ctx.arch, ctx.target)?;
    starry::build::postprocess_starry_artifact(ctx.workspace_root, &request, cargo, output)
}


pub(super) fn maybe_start_starry_future_incompat_report(
    runtime: KtestRuntime,
    target_dir: &Path,
    arch: &str,
) -> anyhow::Result<Option<crate::build::FutureIncompatReportSession>> {
    if runtime != KtestRuntime::Starry || arch != "aarch64" {
        return Ok(None);
    }
    crate::build::start_future_incompat_report_session(target_dir).map(Some)
}


pub(super) fn default_qemu_build_config(package_dir: &Path, target: &str) -> Option<PathBuf> {
    let package_config = package_dir.join(format!("build-{target}.toml"));
    if package_config.exists() {
        Some(package_config)
    } else {
        None
    }
}


pub(super) fn default_qemu_run_config(
    workspace_root: &Path,
    package_dir: &Path,
    runtime: KtestRuntime,
    arch: &str,
) -> PathBuf {
    let package_config = package_dir.join(format!("qemu-{arch}.toml"));
    if package_config.exists() {
        package_config
    } else {
        runtime_config_root(workspace_root, runtime).join(format!("configs/qemu/qemu-{arch}.toml"))
    }
}


pub(super) fn default_board_build_config(package_dir: &Path, board: &str) -> anyhow::Result<Option<PathBuf>> {
    let package_config = package_dir.join(format!("build-{board}.toml"));
    if package_config.exists() {
        return Ok(Some(package_config));
    }
    let mut package_configs = fs::read_dir(package_dir)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("build-") && name.ends_with(".toml"))
        })
        .collect::<Vec<_>>();
    package_configs.sort();
    if let [only] = package_configs.as_slice() {
        return Ok(Some(only.clone()));
    }
    Ok(None)
}


pub(super) fn default_board_run_config(
    workspace_root: &Path,
    package_dir: &Path,
    runtime: KtestRuntime,
    board: &str,
) -> PathBuf {
    let package_config = package_dir.join(format!("board-{board}.toml"));
    if package_config.exists() {
        return package_config;
    }
    let root = runtime_config_root(workspace_root, runtime);
    root.join(format!("configs/board/{board}-board.toml"))
}


pub(super) fn runtime_config_root(workspace_root: &Path, runtime: KtestRuntime) -> PathBuf {
    match runtime {
        KtestRuntime::Arceos | KtestRuntime::Board => workspace_root.join("os/arceos"),
        KtestRuntime::Starry => workspace_root.join("os/StarryOS"),
        KtestRuntime::Axvisor => workspace_root.join("os/axvisor"),
    }
}

