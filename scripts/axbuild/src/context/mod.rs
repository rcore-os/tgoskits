use std::path::{Path, PathBuf};

use ostool::{
    Tool, ToolConfig,
    build::{CargoRunnerKind, config::Cargo},
};

mod arch;
mod resolve;
mod snapshot;
#[cfg(test)]
mod tests;
mod types;
mod workspace;

pub use arch::{
    arch_for_target, arch_for_target_checked, starry_arch_for_target,
    starry_arch_for_target_checked, starry_target_for_arch, starry_target_for_arch_checked,
    target_for_arch, target_for_arch_checked,
};
pub(crate) use arch::{resolve_axvisor_arch_and_target, resolve_starry_arch_and_target};
pub(crate) use resolve::snapshot_path_value;
pub use types::{
    ARCEOS_SNAPSHOT_FILE, AXVISOR_SNAPSHOT_FILE, ArceosCommandSnapshot, ArceosQemuSnapshot,
    ArceosUbootSnapshot, AxvisorCliArgs, AxvisorCommandSnapshot, AxvisorQemuSnapshot,
    AxvisorUbootSnapshot, BuildCliArgs, DEFAULT_ARCEOS_TARGET, DEFAULT_AXVISOR_ARCH,
    DEFAULT_AXVISOR_TARGET, DEFAULT_STARRY_ARCH, DEFAULT_STARRY_TARGET, QemuRunConfig,
    ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest, STARRY_PACKAGE,
    STARRY_SNAPSHOT_FILE, StarryCliArgs, StarryCommandSnapshot, StarryQemuSnapshot,
    StarryUbootSnapshot,
};
pub(crate) use workspace::{find_workspace_root, workspace_member_dir, workspace_root_path};

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    root: PathBuf,
    axvisor_dir: PathBuf,
}

impl AppContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = find_workspace_root();
        let axvisor_dir = workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)?;
        crate::logging::init_logging(&workspace_root)?;

        info!("Workspace root: {}", workspace_root.display());
        info!("Axvisor dir: {}", axvisor_dir.display());

        let tool = Tool::new(ToolConfig::default()).unwrap();
        Ok(Self {
            tool,
            build_config_path: None,
            root: workspace_root,
            axvisor_dir,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub fn axvisor_dir(&self) -> &Path {
        &self.axvisor_dir
    }

    pub async fn build(&mut self, cargo: Cargo, build_config_path: PathBuf) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool.cargo_build(&cargo).await
    }

    pub async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        mut qemu: QemuRunConfig,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        qemu.default_args.to_bin.get_or_insert(cargo.to_bin);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Qemu {
                    qemu_config: qemu.qemu_config,
                    debug: false,
                    dtb_dump: false,
                    default_args: qemu.default_args,
                    append_args: qemu.append_args,
                    override_args: qemu.override_args,
                },
            )
            .await
    }

    pub async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run(&cargo, &CargoRunnerKind::Uboot { uboot_config })
            .await
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path.clone());
        self.tool.ctx_mut().build_config_path = Some(path);
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new().expect("failed to initialize AppContext")
    }
}
