use std::path::{Path, PathBuf};

use ostool::{
    Tool, ToolConfig,
    board::{RunBoardOptions, config::BoardRunConfig},
    build::{
        CargoQemuRunnerArgs, CargoRunnerKind, CargoUbootRunnerArgs,
        config::{BuildConfig, BuildSystem, Cargo},
    },
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
            debug: false,
        })
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.root
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
        self.seed_cargo_tool_context(&cargo, &build_config_path);
        self.tool
            .build_with_config(&BuildConfig {
                system: BuildSystem::Cargo(cargo),
            })
            .await
    }

    pub(crate) async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        qemu: Option<QemuConfig>,
    ) -> anyhow::Result<()> {
        self.seed_cargo_tool_context(&cargo, &build_config_path);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Qemu(CargoQemuRunnerArgs {
                    qemu,
                    debug: self.debug,
                    dtb_dump: false,
                    show_output: true,
                }),
            )
            .await
    }

    pub(crate) async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot: Option<UbootConfig>,
    ) -> anyhow::Result<()> {
        self.seed_cargo_tool_context(&cargo, &build_config_path);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Uboot(CargoUbootRunnerArgs {
                    uboot,
                    show_output: true,
                }),
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
        self.seed_cargo_tool_context(&cargo, &build_config_path);
        self.tool
            .run_board(
                &BuildConfig {
                    system: BuildSystem::Cargo(cargo),
                },
                &board_config,
                options,
            )
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

        if let Some(path) = self.build_config_path.clone() {
            self.tool.ctx_mut().build_config_path = Some(path);
        }

        Ok(())
    }

    pub(crate) async fn load_qemu_config_for_cargo(
        &mut self,
        cargo: &Cargo,
        build_config_path: &Path,
    ) -> anyhow::Result<QemuConfig> {
        self.seed_cargo_tool_context(cargo, build_config_path);
        self.tool.load_qemu_config_for_cargo(cargo).await
    }

    pub(crate) async fn load_qemu_config_from_path(
        &mut self,
        cargo: &Cargo,
        build_config_path: &Path,
        qemu_config_path: &Path,
    ) -> anyhow::Result<QemuConfig> {
        self.seed_cargo_tool_context(cargo, build_config_path);
        self.tool.load_qemu_config_from_path(qemu_config_path).await
    }

    pub(crate) async fn load_uboot_config_from_path(
        &mut self,
        cargo: &Cargo,
        build_config_path: &Path,
        uboot_config_path: &Path,
    ) -> anyhow::Result<UbootConfig> {
        self.seed_cargo_tool_context(cargo, build_config_path);
        self.tool
            .load_uboot_config_from_path(uboot_config_path)
            .await
    }

    pub(crate) async fn load_board_run_config_from_dir(
        &mut self,
        cargo: &Cargo,
        build_config_path: &Path,
        dir: &Path,
    ) -> anyhow::Result<BoardRunConfig> {
        self.seed_cargo_tool_context(cargo, build_config_path);
        self.tool.load_board_run_config_from_dir(dir).await
    }

    pub(crate) async fn load_board_run_config_from_path(
        &mut self,
        cargo: &Cargo,
        build_config_path: &Path,
        board_config_path: &Path,
    ) -> anyhow::Result<BoardRunConfig> {
        self.seed_cargo_tool_context(cargo, build_config_path);
        self.tool
            .load_board_run_config_from_path(board_config_path)
            .await
    }

    fn seed_cargo_tool_context(&mut self, cargo: &Cargo, path: &Path) {
        let build_config = BuildConfig {
            system: BuildSystem::Cargo(cargo.clone()),
        };
        self.set_build_config_path(path.to_path_buf());
        self.tool.ctx_mut().build_config = Some(build_config);
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
