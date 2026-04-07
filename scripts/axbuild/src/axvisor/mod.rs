use std::path::PathBuf;

use anyhow::Context;
use ostool::board::RunBoardArgs;

use crate::{
    axvisor::context::AxvisorContext,
    command_flow::{self, SnapshotPersistence},
    context::{AppContext, AxvisorCliArgs, QemuRunConfig, ResolvedAxvisorRequest},
    test_qemu,
};

pub mod board;
pub mod build;
pub mod cli;
pub mod config;
pub mod context;
pub mod image;
pub mod qemu;
pub mod qemu_test;

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
        self.run_qemu_request(request).await
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
        let cargo = build::load_cargo_config(&request)?;
        self.app
            .board(
                cargo,
                request.build_info_path,
                RunBoardArgs {
                    config: None,
                    board_config: args.board_config,
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

    async fn test_qemu(&mut self, args: cli::ArgsTestQemu) -> anyhow::Result<()> {
        let (arch, target) = test_qemu::parse_axvisor_test_target(&args.target)?;

        println!(
            "running axvisor qemu tests for arch: {} (target: {})",
            arch, target
        );

        let vmconfig = match arch {
            "aarch64" => {
                qemu_test::prepare_linux_aarch64_guest_assets(&self.ctx)
                    .await?
                    .generated_vmconfig
            }
            "x86_64" => qemu_test::prepare_nimbos_x86_64_guest_vmconfig(&self.ctx).await?,
            _ => unreachable!(),
        };

        let request = self.prepare_request(
            qemu_test::qemu_test_build_args(arch, vmconfig),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let qemu_config =
            qemu::default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
        let shell = test_qemu::axvisor_test_shell_config(arch)?;
        let override_args = qemu_test::shell_autoinit_qemu_override_args(&request, &shell)?;

        self.app
            .qemu(
                build::load_cargo_config(&request)?,
                request.build_info_path,
                QemuRunConfig {
                    qemu_config: Some(qemu_config),
                    override_args,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| "axvisor qemu test failed")
    }

    async fn test_uboot(&mut self, args: cli::ArgsTestUboot) -> anyhow::Result<()> {
        let board = test_qemu::axvisor_uboot_board_config(&args.board)?;
        let explicit_uboot_config = args.uboot_config.clone();
        let uboot_config_summary = explicit_uboot_config
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "using ostool default search".to_string());

        if let Some(path) = explicit_uboot_config.as_ref()
            && !path.exists()
        {
            bail!(
                "missing explicit U-Boot config `{}` for axvisor board tests",
                path.display()
            );
        }

        println!(
            "running axvisor uboot test for board: {} with vmconfig: {}",
            board.board, board.vmconfig
        );

        let mut request = self.prepare_request(
            qemu_test::uboot_test_build_args(board.build_config, board.vmconfig),
            None,
            explicit_uboot_config.clone(),
            SnapshotPersistence::Discard,
        )?;
        request.uboot_config = explicit_uboot_config;

        let cargo = build::load_cargo_config(&request)?;
        self.app
            .uboot(cargo, request.build_info_path, request.uboot_config)
            .await
            .with_context(|| {
                format!(
                    "axvisor uboot test failed for board `{}` (build_config={}, vmconfig={}, \
                     uboot_config={})",
                    board.board, board.build_config, board.vmconfig, uboot_config_summary
                )
            })
    }

    async fn test_board(&mut self, args: cli::ArgsTestBoard) -> anyhow::Result<()> {
        if args.board_test_config.is_some() && args.test_group.is_none() {
            bail!(
                "`--board-test-config` requires `--test-group` because board test configs embed a \
                 single board_type"
            );
        }

        if let Some(path) = args.board_test_config.as_ref()
            && !path.exists()
        {
            bail!("missing explicit board test config `{}`", path.display());
        }

        let groups = test_qemu::axvisor_board_test_groups(args.test_group.as_deref())?;
        let total = groups.len();
        let mut failed = Vec::new();

        for (index, group) in groups.into_iter().enumerate() {
            let board_test_config = args
                .board_test_config
                .clone()
                .unwrap_or_else(|| PathBuf::from(group.board_test_config));
            let board_test_config_summary = board_test_config.display().to_string();

            if !board_test_config.exists() {
                eprintln!(
                    "failed: {}: missing board test config `{}`",
                    group.name, board_test_config_summary
                );
                failed.push(group.name.to_string());
                continue;
            }

            println!("[{}/{}] axvisor board {}", index + 1, total, group.name);

            let result = async {
                let request = self.prepare_request(
                    qemu_test::board_test_build_args(&group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                let cargo = build::load_cargo_config(&request)?;
                self.app
                    .board(
                        cargo,
                        request.build_info_path,
                        RunBoardArgs {
                            config: None,
                            board_config: Some(board_test_config.clone()),
                            board_type: args.board_type.clone(),
                            server: args.server.clone(),
                            port: args.port,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "axvisor board test failed for group `{}` (build_config={}, \
                             board_test_config={}, vmconfigs={})",
                            group.name,
                            group.build_config,
                            board_test_config_summary,
                            group.vmconfigs.join(", ")
                        )
                    })
            }
            .await;

            match result {
                Ok(()) => println!("ok: {}", group.name),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", group.name, err);
                    failed.push(group.name.to_string());
                }
            }
        }

        test_qemu::finalize_board_test_run(&failed)
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
        if matches!(persistence, SnapshotPersistence::Store) {
            self.app.store_axvisor_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn qemu_run_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<QemuRunConfig> {
        if let Some(path) = request.qemu_config.clone() {
            Ok(QemuRunConfig {
                qemu_config: Some(path),
                ..Default::default()
            })
        } else {
            qemu::default_qemu_run_config(request)
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        command_flow::run_qemu(
            &mut self.app,
            request,
            build::load_cargo_config,
            Self::qemu_run_config,
        )
        .await
    }

    async fn run_build_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        command_flow::run_uboot(&mut self.app, request, build::load_cargo_config).await
    }
}

impl Default for Axvisor {
    fn default() -> Self {
        Self::new().expect("failed to initialize Axvisor")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ResolvedAxvisorRequest, workspace_member_dir, workspace_root_path};

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
    fn default_qemu_run_config_lets_ostool_resolve_default_path() {
        let run_config = qemu::default_qemu_run_config(&ResolvedAxvisorRequest {
            package: "axvisor".to_string(),
            axvisor_dir: PathBuf::from("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("os/axvisor/.build-aarch64-unknown-none-softfloat.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert_eq!(run_config.qemu_config, None);
        assert!(run_config.default_args.args.is_some());
    }
}
