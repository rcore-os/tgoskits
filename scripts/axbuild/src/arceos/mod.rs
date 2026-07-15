use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use log::warn;
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::{
    context::{AppContext, BuildCliArgs, ResolvedBuildRequest, SnapshotPersistence},
    test::host_http::HostHttpServerGuard,
};

mod board;
pub mod build;
pub mod cbuild;
pub mod config;
pub mod rootfs;
pub mod test;

fn start_qemu_host_http_server(
    request: &ResolvedBuildRequest,
) -> anyhow::Result<Option<HostHttpServerGuard>> {
    request
        .qemu_config
        .as_deref()
        .map(crate::test::qemu::load_qemu_case_host_http_server)
        .transpose()?
        .flatten()
        .map(|config| HostHttpServerGuard::start(&config, &request.package))
        .transpose()
}

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// ArceOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build ArceOS application
    Build(ArgsBuild),
    /// Build and run ArceOS application in QEMU
    Qemu(ArgsQemu),
    /// Generate a default ArceOS dynamic board config
    Defconfig(ArgsDefconfig),
    /// ArceOS board config helpers
    Config(ArgsConfig),
    /// Run ArceOS test suites
    Test(test::ArgsTest),
    /// Build and run ArceOS application with U-Boot
    Uboot(ArgsUboot),
    /// Build and run ArceOS application on a remote board
    Board(ArgsBoard),
}

#[derive(Args)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(short, long)]
    pub package: Option<String>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,

    /// Override the rootfs disk image path (skips auto-download).
    #[arg(long, value_name = "IMAGE")]
    pub rootfs: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsBoard {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long = "board-type")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ArgsDefconfig {
    pub board: String,
}

#[derive(Args)]
pub struct ArgsConfig {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
}

// ---------------------------------------------------------------------------
// ArceOS executor
// ---------------------------------------------------------------------------

pub struct ArceOS {
    pub(super) app: AppContext,
}

impl From<&ArgsBuild> for BuildCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            package: args.package.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            smp: args.smp,
            debug: args.debug,
        }
    }
}

impl ArceOS {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Defconfig(args) => self.defconfig(args),
            Command::Config(args) => self.config(args),
            Command::Uboot(args) => self.uboot(args).await,
            Command::Board(args) => self.board(args).await,
            Command::Test(args) => self.test(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        self.ensure_default_build_config_for_request(&request, "build")?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        self.ensure_default_build_config_for_request(&request, "qemu")?;
        if let Some(rootfs) = args.rootfs {
            rootfs::qemu_with_explicit_rootfs(self, request, rootfs).await
        } else {
            self.run_qemu_request(request).await
        }
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

    fn defconfig(&mut self, args: ArgsDefconfig) -> anyhow::Result<()> {
        let path = config::write_defconfig(self.app.workspace_root(), &args.board)?;
        println!("Generated {} for board {}", path.display(), args.board);
        Ok(())
    }

    fn config(&mut self, args: ArgsConfig) -> anyhow::Result<()> {
        match args.command {
            ConfigCommand::Ls => {
                for board in config::available_board_names(self.app.workspace_root())? {
                    println!("{board}");
                }
            }
        }
        Ok(())
    }

    async fn board(&mut self, args: ArgsBoard) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args.build).into(), None, None, SnapshotPersistence::Store)?;
        self.run_board_request(
            request,
            args.board_config,
            RunBoardOptions {
                board_type: args.board_type,
                server: args.server,
                port: args.port,
            },
        )
        .await
    }

    // ---- test dispatch ----

    async fn test(&mut self, args: test::ArgsTest) -> anyhow::Result<()> {
        test::test(self, args).await
    }

    // ---- internal helpers ----

    pub(super) fn prepare_request(
        &self,
        args: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedBuildRequest> {
        let (request, snapshot) = self.app.prepare_arceos_request(
            args,
            qemu_config,
            uboot_config,
            build::resolve_build_info_path,
        )?;
        if persistence.should_store() {
            self.app.store_arceos_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn ensure_default_build_config_for_request(
        &self,
        request: &ResolvedBuildRequest,
        command: &str,
    ) -> anyhow::Result<()> {
        if let Some(board) = config::ensure_default_build_config_for_target(
            self.app.workspace_root(),
            &request.package,
            &request.target,
            &request.build_info_path,
        )? {
            println!(
                "generated missing ArceOS {command} build config {} from board {}",
                request.build_info_path.display(),
                board.name
            );
        }
        Ok(())
    }

    pub(super) async fn load_qemu_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::qemu::QemuConfig>> {
        let mut qemu = match request.qemu_config.as_deref() {
            Some(path) => self
                .app
                .read_qemu_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some)?,
            None => {
                let path =
                    default_qemu_config_template_path(self.app.workspace_root(), &request.arch);
                self.app
                    .read_qemu_config_from_path_for_cargo(cargo, &path)
                    .await
                    .map(Some)?
            }
        };
        if let Some(qemu) = qemu.as_mut() {
            crate::test::qemu::apply_dynamic_platform_qemu_boot(qemu, cargo);
        }
        Ok(qemu)
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedBuildRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
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
                    .read_board_run_config_from_path_for_cargo(cargo, path)
                    .await
            }
            None => {
                let workspace_root = self.app.workspace_root().to_path_buf();
                self.app
                    .ensure_board_run_config_in_dir_for_cargo(cargo, &workspace_root)
                    .await
            }
        }
    }

    fn validate_board_request(request: &ResolvedBuildRequest) -> anyhow::Result<()> {
        if request.build_info_path.exists() {
            board::load_build_file(&request.build_info_path)?;
        }
        Ok(())
    }

    async fn run_qemu_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let cargo = build::load_cargo_config(&request)?;
                self.run_qemu_request_with_cargo(request, cargo).await
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                self.run_c_app_qemu_request(request, app_dir, app_name)
                    .await
            }
        }
    }

    async fn run_board_request(
        &mut self,
        request: ResolvedBuildRequest,
        board_config_path: Option<PathBuf>,
        options: RunBoardOptions,
    ) -> anyhow::Result<()> {
        self.run_board_request_with_extra_rustflags(request, board_config_path, options, &[])
            .await
    }

    pub(super) async fn run_board_request_with_extra_rustflags(
        &mut self,
        request: ResolvedBuildRequest,
        board_config_path: Option<PathBuf>,
        options: RunBoardOptions,
        extra_rustflags: &[&str],
    ) -> anyhow::Result<()> {
        Self::validate_board_request(&request)?;
        self.app.set_debug_mode(request.debug)?;
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let mut cargo = build::load_cargo_config(&request)?;
                if !extra_rustflags.is_empty() {
                    crate::build::append_encoded_rustflags(&mut cargo, extra_rustflags);
                }
                let board_config = self
                    .load_board_config(&cargo, board_config_path.as_deref())
                    .await?;
                self.app
                    .board(cargo, request.build_info_path, board_config, options)
                    .await
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                if !extra_rustflags.is_empty() {
                    bail!("ArceOS board extra rustflags are only supported for RustStd packages");
                }
                let request = c_app_internal_request(&request);
                let cargo = build::load_c_app_cargo_config(&request)?;
                let board_config = self
                    .load_board_config(&cargo, board_config_path.as_deref())
                    .await?;
                let output = self.build_c_app_request(&request, app_dir, app_name)?;
                self.app
                    .board_prepared_elf(
                        output.elf_path,
                        cargo.to_bin,
                        request.build_info_path,
                        board_config,
                        options,
                    )
                    .await
            }
        }
    }

    async fn run_qemu_request_with_cargo(
        &mut self,
        request: ResolvedBuildRequest,
        cargo: Cargo,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let mut qemu = self
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "missing ArceOS QEMU config for target `{}`; pass --qemu-config or add {}",
                    request.target,
                    default_qemu_config_template_path(self.app.workspace_root(), &request.arch)
                        .display()
                )
            })?;
        // ArceOS currently boots its default QEMU path from a fresh FAT32 image.
        // Keep this distinct from the image-managed rootfs used by StarryOS and
        // Axvisor until their runtime filesystem contracts are unified.
        crate::test::qemu::apply_smp_qemu_arg(&mut qemu, request.smp);
        rootfs::prepare_default_qemu_fat32_rootfs(self.app.workspace_root(), &qemu)?;
        let _host_http_server = start_qemu_host_http_server(&request)?;
        self.app
            .qemu(cargo, request.build_info_path, Some(qemu))
            .await
    }

    async fn run_build_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let cargo = build::load_cargo_config(&request)?;
                self.app
                    .build(cargo, request.build_info_path)
                    .await
                    .map(|_| ())
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                let request = c_app_internal_request(&request);
                let output = self.build_c_app_request(&request, app_dir, app_name)?;
                println!("Built ArceOS C app ELF: {}", output.elf_path.display());
                Ok(())
            }
        }
    }

    async fn run_uboot_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        match build::load_arceos_build_mode(&request.build_info_path)? {
            build::ArceosBuildMode::RustStd => {
                let cargo = build::load_cargo_config(&request)?;
                let uboot = self.load_uboot_config(&request, &cargo).await?;
                self.app.uboot(cargo, request.build_info_path, uboot).await
            }
            build::ArceosBuildMode::AppC { app_dir, app_name } => {
                self.run_c_app_uboot_request(request, app_dir, app_name)
                    .await
            }
        }
    }

    fn build_c_app_request(
        &mut self,
        request: &ResolvedBuildRequest,
        app_dir: PathBuf,
        app_name: String,
    ) -> anyhow::Result<cbuild::ArceosCBuildOutput> {
        let workspace_root = self.app.workspace_root();
        let config = build::load_arceos_build_config(&request.build_info_path)?;
        let paths = cbuild::default_c_app_artifact_paths(workspace_root, &app_name);
        let input = cbuild::ArceosCBuildInput {
            app_dir,
            app_name,
            target_dir: paths.target_dir,
            out_dir: paths.out_dir,
            features: config.build_info.features,
        };

        cbuild::build_c_app(workspace_root, request, &input)
    }

    async fn run_c_app_qemu_request(
        &mut self,
        request: ResolvedBuildRequest,
        app_dir: PathBuf,
        app_name: String,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let request = c_app_internal_request(&request);
        let cargo = build::load_c_app_cargo_config(&request)?;
        let mut qemu = self
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "ArceOS C app config {} requires an explicit qemu config",
                    request.build_info_path.display()
                )
            })?;
        let output = self.build_c_app_request(&request, app_dir, app_name)?;
        crate::test::qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
        // See `run_qemu_request_with_cargo`: default ArceOS QEMU keeps a FAT32 rootfs.
        crate::test::qemu::apply_smp_qemu_arg(&mut qemu, request.smp);
        rootfs::prepare_default_qemu_fat32_rootfs(self.app.workspace_root(), &qemu)?;
        self.app
            .prepare_elf_artifact(output.elf_path, qemu.to_bin)
            .await?;
        let _host_http_server = start_qemu_host_http_server(&request)?;
        self.app.run_prepared_qemu(qemu, None).await
    }

    async fn run_c_app_uboot_request(
        &mut self,
        request: ResolvedBuildRequest,
        app_dir: PathBuf,
        app_name: String,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let request = c_app_internal_request(&request);
        let cargo = build::load_c_app_cargo_config(&request)?;
        let uboot = self
            .load_uboot_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "ArceOS C app config {} requires an explicit uboot config",
                    request.build_info_path.display()
                )
            })?;
        let output = self.build_c_app_request(&request, app_dir, app_name)?;
        self.app.prepare_elf_artifact(output.elf_path, true).await?;
        self.app.run_prepared_uboot(uboot).await
    }
}

fn warn_if_c_app_package_override(request: &ResolvedBuildRequest) {
    if request.package != "ax-libc" {
        warn!(
            "ArceOS C app build ignores --package {}; using ax-libc internally",
            request.package
        );
    }
}

fn c_app_internal_request(request: &ResolvedBuildRequest) -> ResolvedBuildRequest {
    warn_if_c_app_package_override(request);
    let mut request = request.clone();
    request.package = "ax-libc".to_string();
    request
}

pub(crate) fn default_qemu_config_template_path(workspace_root: &Path, arch: &str) -> PathBuf {
    workspace_root.join(format!("os/arceos/configs/qemu/qemu-{arch}.toml"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tempfile::tempdir;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: Command,
    }

    fn parse(args: impl IntoIterator<Item = &'static str>) -> Command {
        Cli::try_parse_from(args).unwrap().command
    }

    #[test]
    fn command_parses_defconfig() {
        match parse(["arceos", "defconfig", "orangepi-5-plus"]) {
            Command::Defconfig(args) => assert_eq!(args.board, "orangepi-5-plus"),
            _ => panic!("expected defconfig command"),
        }
    }

    #[test]
    fn command_parses_config_ls() {
        match parse(["arceos", "config", "ls"]) {
            Command::Config(args) => match args.command {
                ConfigCommand::Ls => {}
            },
            _ => panic!("expected config ls command"),
        }
    }

    #[test]
    fn command_parses_board() {
        match parse([
            "arceos",
            "board",
            "--config",
            "build.toml",
            "--board-config",
            "board.toml",
            "-b",
            "OrangePi-5-Plus",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ]) {
            Command::Board(args) => {
                assert_eq!(args.build.config, Some(PathBuf::from("build.toml")));
                assert_eq!(args.board_config, Some(PathBuf::from("board.toml")));
                assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.port, Some(9000));
            }
            _ => panic!("expected board command"),
        }
    }

    #[test]
    fn standard_x86_64_and_loongarch64_qemu_configs_use_uefi_boot() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        for (config_name, memory) in [
            ("qemu-x86_64.toml", "512M"),
            ("qemu-loongarch64.toml", "2G"),
        ] {
            let config_path = workspace.join("os/arceos/configs/qemu").join(config_name);
            let config: QemuConfig =
                toml::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();

            assert!(config.uefi);
            assert!(config.to_bin);
            assert!(
                config.args.windows(2).any(|args| args == ["-m", memory]),
                "{config_name} must reserve {memory} for UEFI boot"
            );
        }
    }

    #[test]
    fn standard_config_templates_cover_every_supported_qemu_target() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        for (arch, target) in [
            ("aarch64", "aarch64-unknown-none-softfloat"),
            ("x86_64", "x86_64-unknown-none"),
            ("riscv64", "riscv64gc-unknown-none-elf"),
            ("loongarch64", "loongarch64-unknown-none-softfloat"),
        ] {
            let qemu_path = workspace.join(format!("os/arceos/configs/qemu/qemu-{arch}.toml"));
            let qemu: QemuConfig =
                toml::from_str(&std::fs::read_to_string(qemu_path).unwrap()).unwrap();
            assert!(!qemu.args.is_empty());

            let board_path = workspace.join(format!("os/arceos/configs/board/qemu-{arch}.toml"));
            let board = board::load_board_file(&board_path).unwrap();
            assert_eq!(board.package, "arceos-helloworld");
            assert_eq!(board.target, target);
        }
    }

    #[test]
    fn default_qemu_config_template_uses_arceos_config_directory() {
        assert_eq!(
            default_qemu_config_template_path(Path::new("/workspace"), "aarch64"),
            PathBuf::from("/workspace/os/arceos/configs/qemu/qemu-aarch64.toml")
        );
    }

    #[test]
    fn qemu_request_starts_host_http_server_from_config() {
        let root = tempdir().unwrap();
        let qemu_config = root.path().join("qemu-x86_64.toml");
        std::fs::write(
            &qemu_config,
            r#"
args = []

[host_http_server]
port = 0
body = "fixture"
"#,
        )
        .unwrap();
        let request = ResolvedBuildRequest {
            package: "arceos-httpclient".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            smp: Some(1),
            debug: false,
            build_info_path: root.path().join("build.toml"),
            qemu_config: Some(qemu_config),
            uboot_config: None,
        };

        let guard = start_qemu_host_http_server(&request).unwrap();

        assert!(guard.is_some());
    }
}
