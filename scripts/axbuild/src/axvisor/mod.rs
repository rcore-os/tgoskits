use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
    run::qemu::QemuConfig,
};

use crate::{
    axvisor::context::AxvisorContext,
    command_flow::{self, SnapshotPersistence},
    context::{AppContext, AxvisorCliArgs, ResolvedAxvisorRequest},
    test::{board as board_test, case as test_case, qemu as test_qemu},
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
        let explicit_rootfs = args
            .rootfs
            .map(|r| rootfs::resolve_explicit_rootfs(self.app.workspace_root(), &request.arch, r));
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

    async fn test_qemu(&mut self, args: cli::ArgsTestQemu) -> anyhow::Result<()> {
        let (arch, target) = test::parse_target(&args.arch, &args.target)?;
        let cases = test::discover_qemu_cases(
            self.app.workspace_root(),
            &args.test_group,
            &arch,
            args.test_case.as_deref(),
        )?;

        println!(
            "running axvisor qemu tests for arch: {} (target: {}, cases: {})",
            arch,
            target,
            cases.len()
        );

        let build_config = qemu_test_build_config(&cases)?;
        let vmconfigs = qemu_test_vmconfigs(&cases);

        let request = self.prepare_request(
            axvisor_qemu_test_build_args(&arch, build_config, vmconfigs),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        rootfs::ensure_qemu_rootfs_ready(&request, self.app.workspace_root(), None).await?;
        let cargo = build::load_cargo_config(&request)?;
        self.app.set_debug_mode(request.debug)?;
        self.app
            .build(cargo.clone(), request.build_info_path.clone())
            .await
            .context("failed to build shared Axvisor qemu test artifact")?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut failed = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            println!("[{}/{}] axvisor qemu {}", index + 1, total, case.case.name);

            let case_started = Instant::now();
            let result = self
                .run_qemu_case(&request, &cargo, case)
                .await
                .with_context(|| format!("axvisor qemu test failed for case `{}`", case.case.name));
            match result {
                Ok(()) => println!("ok: {} ({:.2?})", case.case.name, case_started.elapsed()),
                Err(err) => {
                    eprintln!("failed: {}: {err:#}", case.case.name);
                    failed.push(case.case.name.clone());
                }
            }
        }

        println!("axvisor qemu total: {:.2?}", suite_started.elapsed());
        test_qemu::finalize_qemu_test_run("axvisor", &failed)
    }

    async fn test_uboot(&mut self, args: cli::ArgsTestUboot) -> anyhow::Result<()> {
        let board = test::uboot_board_config(&args.board, &args.guest)?;
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
            "running axvisor uboot test for board: {} guest: {} with vmconfig: {}",
            board.board, board.guest, board.vmconfig
        );

        let mut request = self.prepare_request(
            axvisor_uboot_test_build_args(board.build_config, board.vmconfig),
            None,
            explicit_uboot_config.clone(),
            SnapshotPersistence::Discard,
        )?;
        request.uboot_config = explicit_uboot_config;

        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app
            .uboot(cargo, request.build_info_path, uboot)
            .await
            .with_context(|| {
                format!(
                    "axvisor uboot test failed for board `{}` guest `{}` (build_config={}, \
                     vmconfig={}, uboot_config={})",
                    board.board,
                    board.guest,
                    board.build_config,
                    board.vmconfig,
                    uboot_config_summary
                )
            })
    }

    async fn test_board(&mut self, args: cli::ArgsTestBoard) -> anyhow::Result<()> {
        let groups = test::discover_board_test_groups(
            self.app.workspace_root(),
            &args.test_group,
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        let total = groups.len();
        let mut failed = Vec::new();

        for (index, group) in groups.into_iter().enumerate() {
            let group_label = format!("{}/{}", group.name, group.board_name);
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();

            if !board_test_config.exists() {
                eprintln!(
                    "failed: {}: missing board test config `{}`",
                    group_label, board_test_config_summary
                );
                failed.push(group_label);
                continue;
            }

            println!("[{}/{}] axvisor board {}", index + 1, total, group_label);

            let result = async {
                let prepared_vmconfigs = group.vmconfigs.clone();
                let request = self.prepare_request(
                    axvisor_board_test_build_args(&group, prepared_vmconfigs),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                let cargo = build::load_cargo_config(&request)?;
                let board_config = self
                    .load_board_config(&cargo, Some(board_test_config.as_path()))
                    .await?;
                self.app
                    .board(
                        cargo,
                        request.build_info_path,
                        board_config,
                        RunBoardOptions {
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
                            group_label,
                            group.build_config.display(),
                            board_test_config_summary,
                            group
                                .vmconfigs
                                .iter()
                                .map(|path| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
            }
            .await;

            match result {
                Ok(()) => println!("ok: {}", group_label),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", group_label, err);
                    failed.push(group_label);
                }
            }
        }

        board_test::finalize_board_test_run("axvisor", &failed)
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

    async fn load_qemu_case_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
        case: &test::AxvisorQemuCase,
    ) -> anyhow::Result<QemuConfig> {
        let mut qemu = self
            .app
            .tool_mut()
            .read_qemu_config_from_path_for_cargo(cargo, &case.case.qemu_config_path)
            .await?;
        test_case::apply_grouped_qemu_config(&mut qemu, &case.case);

        let mut case_request = request.clone();
        case_request.vmconfigs = case.vmconfigs.clone();
        let rootfs_path = rootfs::qemu_rootfs_path(&case_request, self.app.workspace_root(), None)?;
        let prepared_assets = test_case::prepare_case_assets(
            self.app.workspace_root(),
            &case_request.arch,
            &case_request.target,
            &case.case,
            rootfs_path,
        )
        .await?;
        rootfs::patch_qemu_rootfs_path(&mut qemu, &prepared_assets.rootfs_path);
        qemu.args.extend(prepared_assets.extra_qemu_args);
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
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app.uboot(cargo, request.build_info_path, uboot).await
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
        case: &test::AxvisorQemuCase,
    ) -> anyhow::Result<()> {
        let qemu = self.load_qemu_case_config(request, cargo, case).await?;
        self.app.run_qemu(cargo, qemu).await
    }
}

fn axvisor_qemu_test_build_args(
    arch: &str,
    config: Option<PathBuf>,
    vmconfigs: Vec<PathBuf>,
) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config,
        arch: Some(arch.to_string()),
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs,
    }
}

fn qemu_test_build_config(cases: &[test::AxvisorQemuCase]) -> anyhow::Result<Option<PathBuf>> {
    let mut build_config: Option<PathBuf> = None;
    for case in cases {
        let Some(next) = &case.build_config else {
            continue;
        };
        if let Some(current) = &build_config
            && current != next
        {
            anyhow::bail!(
                "Axvisor qemu cases in one run must use the same build_config for build-once; \
                 `{}` uses `{}`, but an earlier case uses `{}`",
                case.case.name,
                next.display(),
                current.display()
            );
        }
        build_config = Some(next.clone());
    }
    Ok(build_config)
}

fn qemu_test_vmconfigs(cases: &[test::AxvisorQemuCase]) -> Vec<PathBuf> {
    let mut vmconfigs = Vec::new();
    for case in cases {
        for vmconfig in &case.vmconfigs {
            if !vmconfigs.contains(vmconfig) {
                vmconfigs.push(vmconfig.clone());
            }
        }
    }
    vmconfigs
}

fn axvisor_uboot_test_build_args(build_config: &str, vmconfig: &str) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(PathBuf::from(build_config)),
        arch: None,
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs: vec![PathBuf::from(vmconfig)],
    }
}

fn axvisor_board_test_build_args(
    group: &test::BoardTestGroup,
    vmconfigs: Vec<PathBuf>,
) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(group.build_config.clone()),
        arch: None,
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs,
    }
}

impl Default for Axvisor {
    fn default() -> Self {
        Self::new().expect("failed to initialize Axvisor")
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
