use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use ostool::{
    Tool, ToolConfig,
    board::{RunBoardOptions, config::BoardRunConfig},
    build::{CargoQemuRunnerArgs, CargoRunnerKind, CargoUbootRunnerArgs, config::Cargo},
    run::{qemu::QemuConfig, uboot::UbootConfig},
};

mod arch;
mod resolve;
mod snapshot;
#[cfg(test)]
mod tests;
mod types;
mod workspace;

pub(crate) use arch::{
    arch_for_target_checked, resolve_arceos_arch_and_target, resolve_axvisor_arch_and_target,
    resolve_starry_arch_and_target, starry_arch_for_target_checked, starry_target_for_arch_checked,
    target_for_arch_checked,
};
pub(crate) use resolve::snapshot_path_value;
pub use types::{
    ARCEOS_SNAPSHOT_FILE, AXVISOR_SNAPSHOT_FILE, ArceosCommandSnapshot, ArceosQemuSnapshot,
    ArceosUbootSnapshot, AxvisorCliArgs, AxvisorCommandSnapshot, AxvisorQemuSnapshot,
    AxvisorUbootSnapshot, BuildCliArgs, DEFAULT_ARCEOS_ARCH, DEFAULT_ARCEOS_TARGET,
    DEFAULT_AXVISOR_ARCH, DEFAULT_AXVISOR_TARGET, DEFAULT_STARRY_ARCH, DEFAULT_STARRY_TARGET,
    ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest, STARRY_PACKAGE,
    STARRY_SNAPSHOT_FILE, StarryCliArgs, StarryCommandSnapshot, StarryQemuSnapshot,
    StarryUbootSnapshot,
};
pub(crate) use workspace::{
    find_workspace_root, workspace_manifest_path, workspace_member_dir, workspace_member_dir_in,
    workspace_metadata_root_manifest, workspace_root_path,
};

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    root: PathBuf,
    axvisor_dir: Option<PathBuf>,
    original_path: OsString,
    debug: bool,
}

impl AppContext {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let workspace_root = find_workspace_root();
        crate::logging::init_logging(&workspace_root)?;

        info!("Workspace root: {}", workspace_root.display());

        let tool = Tool::new(ToolConfig::default()).unwrap();
        Ok(Self {
            tool,
            build_config_path: None,
            root: workspace_root,
            axvisor_dir: None,
            original_path: env::var_os("PATH").unwrap_or_default(),
            debug: false,
        })
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn tool_mut(&mut self) -> &mut Tool {
        &mut self.tool
    }

    pub(crate) fn axvisor_dir(&mut self) -> anyhow::Result<&Path> {
        if self.axvisor_dir.is_none() {
            let axvisor_dir = workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)?;
            info!("Axvisor dir: {}", axvisor_dir.display());
            self.axvisor_dir = Some(axvisor_dir);
        }

        Ok(self
            .axvisor_dir
            .as_deref()
            .expect("axvisor_dir should be initialized"))
    }

    pub(crate) async fn build(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool.cargo_build(&cargo).await
    }

    pub(crate) async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        qemu: Option<QemuConfig>,
    ) -> anyhow::Result<()> {
        self.restore_original_path();
        if should_use_loongarch_lvz_for(&cargo.package, &cargo.target) {
            configure_loongarch_qemu_path(&self.root)?;
        }
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Qemu(Box::new(CargoQemuRunnerArgs {
                    qemu,
                    debug: self.debug,
                    dtb_dump: false,
                    show_output: true,
                })),
            )
            .await
    }

    pub(crate) async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot: Option<UbootConfig>,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Uboot(Box::new(CargoUbootRunnerArgs {
                    uboot,
                    show_output: true,
                })),
            )
            .await
    }

    pub(crate) async fn board(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        board_config: BoardRunConfig,
        options: RunBoardOptions,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run_board(&cargo, &board_config, options)
            .await
    }

    pub(crate) fn set_debug_mode(&mut self, debug: bool) -> anyhow::Result<()> {
        if self.debug == debug {
            return Ok(());
        }

        self.tool = Tool::new(ToolConfig {
            debug,
            ..ToolConfig::default()
        })?;
        self.debug = debug;

        self.tool
            .set_build_config_path(self.build_config_path.clone());

        Ok(())
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path.clone());
        self.tool.set_build_config_path(Some(path));
    }

    fn restore_original_path(&self) {
        unsafe {
            env::set_var("PATH", &self.original_path);
        }
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new().expect("failed to initialize AppContext")
    }
}

fn should_use_loongarch_lvz_for(package: &str, target: &str) -> bool {
    package == crate::axvisor::build::AXVISOR_PACKAGE && target.contains("loongarch64")
}

fn configure_loongarch_qemu_path(workspace_root: &Path) -> anyhow::Result<()> {
    let Some(qemu_dir) = find_loongarch_qemu_dir(workspace_root) else {
        return Ok(());
    };

    prepend_dir_to_path(&qemu_dir)?;
    info!(
        "Using LoongArch QEMU from PATH-prepended directory: {}",
        qemu_dir.display()
    );
    Ok(())
}

fn find_loongarch_qemu_dir(workspace_root: &Path) -> Option<PathBuf> {
    let env_executable = env::var_os("AXBUILD_QEMU_SYSTEM_LOONGARCH64")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .and_then(|path| path.parent().map(Path::to_path_buf));
    if let Some(dir) = env_executable.filter(|dir| is_loongarch_qemu_dir(dir)) {
        return Some(dir);
    }

    let env_dir = env::var_os("AXBUILD_QEMU_DIR")
        .map(PathBuf::from)
        .filter(|dir| is_loongarch_qemu_dir(dir));
    if let Some(dir) = env_dir {
        return Some(dir);
    }

    loongarch_qemu_dir_candidates(workspace_root)
        .into_iter()
        .find(|dir| is_loongarch_qemu_dir(dir))
}

fn loongarch_qemu_dir_candidates(workspace_root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for suffix in ["QEMU-LVZ/build", "qemu-lvz/build"] {
            candidates.push(home.join(suffix));
        }
    }

    for ancestor in workspace_root.ancestors() {
        for suffix in ["QEMU-LVZ/build", "qemu-lvz/build"] {
            candidates.push(ancestor.join(suffix));
        }
    }

    candidates
}

fn is_loongarch_qemu_dir(dir: &Path) -> bool {
    dir.join("qemu-system-loongarch64").exists()
}

fn prepend_dir_to_path(dir: &Path) -> anyhow::Result<()> {
    let current = env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = env::split_paths(&current).collect();
    if paths.iter().any(|path| path == dir) {
        return Ok(());
    }

    paths.insert(0, dir.to_path_buf());
    let joined = env::join_paths(paths.iter())
        .map_err(|err| anyhow::anyhow!("failed to update PATH with {}: {err}", dir.display()))?;

    unsafe {
        env::set_var("PATH", joined);
    }
    Ok(())
}
