use std::path::{Path, PathBuf};

use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
    run::qemu::QemuConfig,
};

use crate::{
    axvisor::context::AxvisorContext,
    context::{AppContext, AxvisorCliArgs, ResolvedAxvisorRequest, SnapshotPersistence},
};

pub mod board;
pub mod build;
pub mod cli;
pub mod config;
pub mod context;
pub mod image;
pub mod rootfs;
pub mod test;

pub use cli::{
    ArgsBoard, ArgsBuild, ArgsConfig, ArgsDefconfig, ArgsQemu, ArgsTest, ArgsUboot, Command,
    ConfigCommand, TestCommand,
};

pub struct Axvisor {
    app: AppContext,
    ctx: AxvisorContext,
}

impl Axvisor {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        let ctx = AxvisorContext::new()?;
        Ok(Self { app, ctx })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Board(args) => self.board(args).await,
            Command::Defconfig(args) => self.defconfig(args),
            Command::Config(args) => self.config(args),
            Command::Image(args) => self.image(args).await,
            Command::Test(args) => self.test(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let explicit_rootfs = args.rootfs.map(|r| {
            crate::rootfs::store::resolve_explicit_rootfs(
                self.app.workspace_root(),
                &request.arch,
                r,
            )
        });
        rootfs::ensure_qemu_rootfs_ready(
            &request,
            self.app.workspace_root(),
            explicit_rootfs.as_deref(),
        )
        .await?;
        let qemu = self
            .load_qemu_config(&request, &cargo, explicit_rootfs.as_deref())
            .await?;
        self.app
            .qemu(cargo, request.build_info_path, Some(qemu))
            .await
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            None,
            args.uboot_config,
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    async fn board(&mut self, args: ArgsBoard) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args.build).into(), None, None, SnapshotPersistence::Store)?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let board_config = self
            .load_board_config(&cargo, args.board_config.as_deref())
            .await?;
        self.app
            .board(
                cargo,
                request.build_info_path,
                board_config,
                RunBoardOptions {
                    board_type: args.board_type,
                    server: args.server,
                    port: args.port,
                },
            )
            .await
    }

    fn defconfig(&mut self, args: ArgsDefconfig) -> anyhow::Result<()> {
        let workspace_root = self.app.workspace_root().to_path_buf();
        let axvisor_dir = self.app.axvisor_dir()?.to_path_buf();
        let path = config::write_defconfig(&workspace_root, &axvisor_dir, &args.board)?;
        println!("Generated {} for board {}", path.display(), args.board);
        Ok(())
    }

    fn config(&mut self, args: ArgsConfig) -> anyhow::Result<()> {
        match args.command {
            ConfigCommand::Ls => {
                for board in config::available_board_names(self.app.axvisor_dir()?)? {
                    println!("{board}");
                }
            }
        }
        Ok(())
    }

    async fn image(&self, args: image::Args) -> anyhow::Result<()> {
        image::run(args, &self.ctx).await
    }

    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        match args.command {
            TestCommand::Qemu(args) => self.test_qemu(args).await,
            TestCommand::Uboot(args) => self.test_uboot(args).await,
            TestCommand::Board(args) => self.test_board(args).await,
        }
    }

    fn prepare_request(
        &mut self,
        args: AxvisorCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedAxvisorRequest> {
        let (request, snapshot) =
            self.app
                .prepare_axvisor_request(args, qemu_config, uboot_config)?;
        if persistence.should_store() {
            self.app.store_axvisor_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    async fn load_qemu_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
        explicit_rootfs: Option<&Path>,
    ) -> anyhow::Result<QemuConfig> {
        let config_path = request.qemu_config.clone().unwrap_or_else(|| {
            default_qemu_config_template_path(&request.axvisor_dir, &request.arch)
        });
        let mut qemu = self
            .app
            .tool_mut()
            .read_qemu_config_from_path_for_cargo(cargo, &config_path)
            .await?;
        rootfs::patch_qemu_rootfs(
            &mut qemu,
            request,
            self.app.workspace_root(),
            explicit_rootfs,
        )?;
        Ok(qemu)
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
                .read_uboot_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn load_board_config(
        &mut self,
        cargo: &Cargo,
        board_config_path: Option<&Path>,
    ) -> anyhow::Result<BoardRunConfig> {
        match board_config_path {
            Some(path) => {
                self.app
                    .tool_mut()
                    .read_board_run_config_from_path_for_cargo(cargo, path)
                    .await
            }
            None => {
                let workspace_root = self.app.workspace_root().to_path_buf();
                self.app
                    .tool_mut()
                    .ensure_board_run_config_in_dir_for_cargo(cargo, &workspace_root)
                    .await
            }
        }
    }

    async fn run_build_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app.uboot(cargo, request.build_info_path, uboot).await
    }
}

fn default_qemu_config_template_path(axvisor_dir: &Path, arch: &str) -> PathBuf {
    axvisor_dir.join(format!("scripts/ostool/qemu-{arch}.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{workspace_member_dir, workspace_root_path};

    #[test]
    fn context_resolves_workspace_root() {
        let ctx = AxvisorContext::new().unwrap();
        assert_eq!(
            ctx.workspace_root(),
            workspace_root_path().unwrap().as_path()
        );
        assert_eq!(
            ctx.axvisor_dir(),
            workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)
                .unwrap()
                .as_path()
        );
    }

    #[test]
    fn default_qemu_template_path_uses_axvisor_script_location() {
        let path = default_qemu_config_template_path(Path::new("os/axvisor"), "aarch64");

        assert_eq!(
            path,
            PathBuf::from("os/axvisor/scripts/ostool/qemu-aarch64.toml")
        );
    }
}
