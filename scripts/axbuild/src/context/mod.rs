use std::path::{Path, PathBuf};

use ostool::{
    Tool, ToolConfig,
    board::{
        self as ostool_board, client::BoardServerClient, config::BoardRunConfig,
        session::BoardSession,
    },
    build::{
        CargoQemuRunnerArgs, CargoRunnerKind, CargoUbootRunnerArgs,
        cargo_builder::CargoBuilder,
        config::{BuildConfig, BuildSystem, Cargo},
    },
};

mod arch;
mod resolve;
mod snapshot;
#[cfg(test)]
mod tests;
mod types;
mod workspace;

pub(crate) use arch::{
    arch_for_target_checked, resolve_axvisor_arch_and_target, resolve_starry_arch_and_target,
    starry_arch_for_target_checked, starry_target_for_arch_checked, target_for_arch_checked,
};
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
pub(crate) use workspace::{
    find_workspace_root, workspace_member_dir, workspace_member_dir_in, workspace_root_path,
};

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    root: PathBuf,
    axvisor_dir: Option<PathBuf>,
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
        self.set_cargo_build_context(&cargo, build_config_path);
        self.tool.cargo_build(&cargo).await
    }

    pub(crate) async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        mut qemu: QemuRunConfig,
    ) -> anyhow::Result<()> {
        self.set_cargo_build_context(&cargo, build_config_path);
        qemu.default_args.to_bin.get_or_insert(cargo.to_bin);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Qemu(Box::new(CargoQemuRunnerArgs {
                    qemu_config: qemu.qemu_config,
                    debug: false,
                    dtb_dump: false,
                    default_args: qemu.default_args,
                    append_args: qemu.append_args,
                    override_args: qemu.override_args,
                })),
            )
            .await
    }

    pub(crate) async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.set_cargo_build_context(&cargo, build_config_path);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Uboot(CargoUbootRunnerArgs { uboot_config }),
            )
            .await
    }

    pub(crate) async fn board_ls(&self, server: &str, port: u16) -> anyhow::Result<()> {
        ostool_board::list_boards(server, port).await
    }

    pub(crate) async fn board_run(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        board_config: Option<PathBuf>,
        server: Option<&str>,
        port: Option<u16>,
    ) -> anyhow::Result<()> {
        self.set_cargo_build_context(&cargo, build_config_path.clone());

        CargoBuilder::build(&mut self.tool, &cargo, Some(build_config_path))
            .skip_objcopy(true)
            .resolve_artifact_from_json(true)
            .execute()
            .await?;

        let board_config = BoardRunConfig::load_or_create(&self.tool, board_config).await?;
        let (server, port) = board_config.resolve_server(server, port);
        let client = BoardServerClient::new(&server, port)?;
        let session = BoardSession::acquire(client.clone(), &board_config.board_type).await?;

        println!("Allocated board session:");
        println!("  board_type: {}", board_config.board_type);
        println!("  board_id: {}", session.info().board_id);
        println!("  session_id: {}", session.info().session_id);
        println!("  lease_expires_at: {}", session.info().lease_expires_at);
        println!("  boot_mode: {}", session.info().boot_mode);

        let run_result = match session.info().boot_mode.as_str() {
            "uboot" => {
                self.tool
                    .run_uboot_remote(&board_config, client, session.info().clone())
                    .await
            }
            other => Err(anyhow!(
                "unsupported board boot mode `{other}`; only `uboot` is supported"
            )),
        };

        let release_result = session.release().await;
        match (run_result, release_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), Ok(())) => Err(err),
            (Ok(()), Err(err)) => Err(err),
            (Err(run_err), Err(release_err)) => Err(run_err.context(format!(
                "additionally failed to release board session: {release_err:#}"
            ))),
        }
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path.clone());
        self.tool.ctx_mut().build_config_path = Some(path);
    }

    fn set_cargo_build_context(&mut self, cargo: &Cargo, path: PathBuf) {
        self.set_build_config_path(path);
        self.tool.ctx_mut().build_config = Some(BuildConfig {
            system: BuildSystem::Cargo(cargo.clone()),
        });
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new().expect("failed to initialize AppContext")
    }
}
