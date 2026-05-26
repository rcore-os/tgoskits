//! Main ostool CLI argument parsing and command dispatch.

use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::*;
use colored::Colorize as _;
use env_logger::Env;

use log::info;
use ostool::{
    ManifestContext, Tool, ToolConfig, board,
    build::{self, CargoQemuRunnerArgs, CargoRunnerKind, CargoUbootRunnerArgs},
    menuconfig::{MenuConfigHandler, MenuConfigMode},
    resolve_manifest_context,
    run::{
        qemu::{QemuConfig, RunQemuOptions},
        uboot::{RunUbootOptions, UbootConfig},
    },
};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    manifest: Option<PathBuf>,
    #[command(subcommand)]
    command: SubCommands,
}

#[derive(Subcommand, Debug)]
enum SubCommands {
    Build {
        /// Path to the build configuration file
        #[arg(short, long)]
        config: Option<PathBuf>,
        #[command(flatten)]
        cargo_selector: CargoSelectorArgs,
    },
    Run {
        #[command(subcommand)]
        command: RunSubCommands,
    },
    Board(BoardArgs),
    Menuconfig {
        /// Menu configuration mode (qemu or uboot)
        #[arg(value_enum)]
        mode: Option<MenuConfigMode>,
    },
}

#[derive(Args, Debug, Default, Clone)]
struct BoardServerArgs {
    /// ostool-server host
    #[arg(long)]
    server: Option<String>,
    /// ostool-server port
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Args, Debug)]
struct BoardArgs {
    #[command(subcommand)]
    command: BoardSubCommands,
}

#[derive(Subcommand, Debug)]
enum BoardSubCommands {
    Ls(BoardServerArgs),
    Connect(BoardConnectArgs),
    Run(BoardRunArgs),
    Config,
}

#[derive(Args, Debug)]
struct RunQemuCommand {
    /// Path to the build configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[command(flatten)]
    cargo_selector: CargoSelectorArgs,
    #[command(flatten)]
    qemu: QemuArgs,
}

#[derive(Args, Debug)]
struct RunUbootCommand {
    /// Path to the build configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[command(flatten)]
    cargo_selector: CargoSelectorArgs,
    #[command(flatten)]
    uboot: UbootArgs,
}

#[derive(Args, Debug, Default, Clone)]
struct CargoSelectorArgs {
    /// Override the Cargo package from the build configuration
    #[arg(long)]
    package: Option<String>,
    /// Select a Cargo binary target within the selected package
    #[arg(long)]
    bin: Option<String>,
}

impl CargoSelectorArgs {
    fn is_empty(&self) -> bool {
        self.package.is_none() && self.bin.is_none()
    }
}

#[derive(Args, Debug)]
struct BoardRunArgs {
    /// Path to the build configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[command(flatten)]
    cargo_selector: CargoSelectorArgs,
    /// Path to the board runner configuration file, defaults to `pwd/.board.toml`
    #[arg(long = "board-config")]
    board_config: Option<PathBuf>,
    /// Override board type from the board runner configuration
    #[arg(short = 'b', long)]
    board_type: Option<String>,
    #[command(flatten)]
    server: BoardServerArgs,
}

#[derive(Args, Debug)]
struct BoardConnectArgs {
    /// Board type to allocate and connect
    #[arg(short = 'b', long)]
    board_type: String,
    #[command(flatten)]
    server: BoardServerArgs,
}

#[derive(Subcommand, Debug)]
enum RunSubCommands {
    Qemu(RunQemuCommand),
    Uboot(RunUbootCommand),
}

#[derive(Args, Debug, Default)]
pub struct QemuArgs {
    /// Path to the qemu configuration file
    ///
    /// Default behavior when not specified:
    /// - Cargo build system: use the target package directory
    /// - Custom build system: use the workspace directory
    /// - With architecture detected: .qemu-{arch}.toml (e.g., .qemu-aarch64.toml)
    /// - Without architecture: .qemu.toml
    #[arg(short, long)]
    qemu_config: Option<PathBuf>,
    #[arg(short, long)]
    debug: bool,
    /// Dump DTB file
    #[arg(long)]
    dtb_dump: bool,
}

#[derive(Args, Debug)]
pub struct UbootArgs {
    /// Path to the uboot configuration file, default to '.uboot.toml'
    #[arg(short, long)]
    uboot_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match try_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report_error(&err);
            ExitCode::FAILURE
        }
    }
}

/// Parses the CLI and dispatches the selected ostool subcommand.
async fn try_main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let Cli { manifest, command } = Cli::parse();

    match command {
        SubCommands::Board(args) => match args.command {
            BoardSubCommands::Ls(server) => {
                let global_config = board::load_board_global_config_with_notice()?;
                let (server, port) =
                    global_config.resolve_server(server.server.as_deref(), server.port);
                board::list_boards(&server, port).await?;
            }
            BoardSubCommands::Connect(args) => {
                let global_config = board::load_board_global_config_with_notice()?;
                let (server, port) =
                    global_config.resolve_server(args.server.server.as_deref(), args.server.port);
                board::connect_board(&server, port, &args.board_type).await?;
            }
            BoardSubCommands::Run(args) => {
                let (mut tool, manifest_ctx) = init_tool(manifest.clone())?;
                let mut build_config =
                    load_build_config(&mut tool, &manifest_ctx, args.config.as_deref()).await?;
                apply_cargo_selector(&mut tool, &mut build_config, &args.cargo_selector)?;
                let board_config =
                    load_board_config(&mut tool, &manifest_ctx, args.board_config.as_deref())
                        .await?;
                tool.run_board(
                    &build_config,
                    &board_config,
                    board::RunBoardOptions {
                        board_type: args.board_type,
                        server: args.server.server,
                        port: args.server.port,
                    },
                )
                .await?;
            }
            BoardSubCommands::Config => {
                board::config()?;
            }
        },
        SubCommands::Build {
            config,
            cargo_selector,
        } => {
            let (mut tool, manifest_ctx) = init_tool(manifest)?;
            let mut build_config =
                load_build_config(&mut tool, &manifest_ctx, config.as_deref()).await?;
            apply_cargo_selector(&mut tool, &mut build_config, &cargo_selector)?;
            tool.build_with_config(&build_config).await?;
        }
        SubCommands::Run { command } => match command {
            RunSubCommands::Qemu(args) => {
                let RunQemuCommand {
                    config,
                    cargo_selector,
                    qemu,
                } = args;
                let debug = qemu.debug;
                let dtb_dump = qemu.dtb_dump;

                let (mut tool, manifest_ctx) = init_tool(manifest.clone())?;
                let mut build_config =
                    load_build_config(&mut tool, &manifest_ctx, config.as_deref()).await?;
                apply_cargo_selector(&mut tool, &mut build_config, &cargo_selector)?;
                match &build_config.system {
                    build::config::BuildSystem::Cargo(config) => {
                        let qemu_config = match qemu.qemu_config.as_deref() {
                            Some(path) => Some(
                                tool.read_qemu_config_from_path_for_cargo(config, path)
                                    .await?,
                            ),
                            None => None,
                        };
                        let kind = CargoRunnerKind::new_qemu(CargoQemuRunnerArgs {
                            qemu: qemu_config,
                            debug,
                            dtb_dump,
                            show_output: true,
                        });
                        tool.cargo_run(config, &kind).await?;
                    }
                    build::config::BuildSystem::Custom(custom_cfg) => {
                        tool.build_with_config(&build_config).await?;
                        tool.prepare_elf_artifact(
                            custom_cfg.elf_path.clone().into(),
                            custom_cfg.to_bin,
                        )
                        .await?;
                        let qemu_config =
                            load_qemu_config(&mut tool, &manifest_ctx, qemu.qemu_config.as_deref())
                                .await?;
                        tool.run_qemu(
                            &qemu_config,
                            RunQemuOptions {
                                dtb_dump,
                                show_output: true,
                            },
                        )
                        .await?;
                    }
                }
            }
            RunSubCommands::Uboot(args) => {
                let RunUbootCommand {
                    config,
                    cargo_selector,
                    uboot,
                } = args;

                let (mut tool, manifest_ctx) = init_tool(manifest.clone())?;
                let mut build_config =
                    load_build_config(&mut tool, &manifest_ctx, config.as_deref()).await?;
                apply_cargo_selector(&mut tool, &mut build_config, &cargo_selector)?;
                match &build_config.system {
                    build::config::BuildSystem::Cargo(config) => {
                        let uboot_config = match uboot.uboot_config.as_deref() {
                            Some(path) => Some(
                                tool.read_uboot_config_from_path_for_cargo(config, path)
                                    .await?,
                            ),
                            None => None,
                        };
                        let kind = CargoRunnerKind::new_uboot(CargoUbootRunnerArgs {
                            uboot: uboot_config,
                            show_output: true,
                        });
                        tool.cargo_run(config, &kind).await?;
                    }
                    build::config::BuildSystem::Custom(custom_cfg) => {
                        tool.build_with_config(&build_config).await?;
                        tool.prepare_elf_artifact(
                            custom_cfg.elf_path.clone().into(),
                            custom_cfg.to_bin,
                        )
                        .await?;
                        let uboot_config = load_uboot_config(
                            &mut tool,
                            &manifest_ctx,
                            uboot.uboot_config.as_deref(),
                        )
                        .await?;
                        tool.run_uboot(&uboot_config, RunUbootOptions { show_output: true })
                            .await?;
                    }
                }
            }
        },
        SubCommands::Menuconfig { mode } => {
            let (mut tool, _) = init_tool(manifest)?;
            MenuConfigHandler::handle_menuconfig(&mut tool, mode).await?;
        }
    }

    Ok(())
}

/// Creates the legacy tool facade from an optional manifest argument.
fn init_tool(manifest_arg: Option<PathBuf>) -> Result<(Tool, ManifestContext)> {
    let manifest = resolve_manifest_context(manifest_arg.clone())?;
    info!("Using manifest {}", manifest.manifest_path.display());

    let tool = Tool::new(ToolConfig {
        manifest: Some(manifest.manifest_path.clone()),
        ..Default::default()
    })?;
    Ok((tool, manifest))
}

/// Loads the build config from an explicit path or workspace default.
async fn load_build_config(
    tool: &mut Tool,
    manifest: &ManifestContext,
    config_path: Option<&std::path::Path>,
) -> Result<build::config::BuildConfig> {
    match config_path {
        Some(path) => tool.load_build_config_from_path(path, false).await,
        None => {
            tool.load_build_config_from_dir(&manifest.workspace_dir, false)
                .await
        }
    }
}

/// Applies `--package` and `--bin` overrides to Cargo build configs.
fn apply_cargo_selector(
    tool: &mut Tool,
    build_config: &mut build::config::BuildConfig,
    selector: &CargoSelectorArgs,
) -> Result<()> {
    if selector.is_empty() {
        return Ok(());
    }

    let build::config::BuildSystem::Cargo(cargo) = &mut build_config.system else {
        anyhow::bail!("--package/--bin can only be used with system.Cargo build configs");
    };

    if let Some(package) = &selector.package {
        cargo.package = package.clone();
    }
    if let Some(bin) = &selector.bin {
        cargo.bin = Some(bin.clone());
    }

    tool.ctx_mut().build_config = Some(build_config.clone());
    Ok(())
}

/// Loads QEMU config from an explicit path or workspace default.
async fn load_qemu_config(
    tool: &mut Tool,
    manifest: &ManifestContext,
    config_path: Option<&std::path::Path>,
) -> Result<QemuConfig> {
    match config_path {
        Some(path) => tool.read_qemu_config_from_path(path).await,
        None => {
            tool.ensure_qemu_config_in_dir(&manifest.workspace_dir)
                .await
        }
    }
}

/// Loads U-Boot config from an explicit path or workspace default.
async fn load_uboot_config(
    tool: &mut Tool,
    manifest: &ManifestContext,
    config_path: Option<&std::path::Path>,
) -> Result<UbootConfig> {
    match config_path {
        Some(path) => tool.read_uboot_config_from_path(path).await,
        None => {
            tool.ensure_uboot_config_in_dir(&manifest.workspace_dir)
                .await
        }
    }
}

/// Loads board-run config from an explicit path or workspace default.
async fn load_board_config(
    tool: &mut Tool,
    manifest: &ManifestContext,
    config_path: Option<&std::path::Path>,
) -> Result<board::config::BoardRunConfig> {
    match config_path {
        Some(path) => tool.read_board_run_config_from_path(path).await,
        None => {
            tool.ensure_board_run_config_in_dir(&manifest.workspace_dir)
                .await
        }
    }
}

/// Prints CLI errors with a structured trace.
fn report_error(err: &anyhow::Error) {
    log::error!("{err:#}");
    log::error!("Trace:\n{err:?}");

    println!("{}", format!("Error: {err:#}").red().bold());
    println!("{}", format!("\nTrace:\n{err:?}").red());
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;
    use ostool::{Tool, ToolConfig};

    use super::{
        BoardArgs, BoardSubCommands, CargoSelectorArgs, Cli, SubCommands, apply_cargo_selector,
        build,
    };

    #[test]
    fn parse_board_ls_with_server_args() {
        let cli = Cli::try_parse_from([
            "ostool", "board", "ls", "--server", "10.0.0.2", "--port", "9000",
        ])
        .unwrap();

        match cli.command {
            SubCommands::Board(BoardArgs {
                command: BoardSubCommands::Ls(server),
            }) => {
                assert_eq!(server.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(server.port, Some(9000));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_board_connect_with_short_board_type() {
        let cli = Cli::try_parse_from(["ostool", "board", "connect", "-b", "rk3568"]).unwrap();

        match cli.command {
            SubCommands::Board(BoardArgs {
                command: BoardSubCommands::Connect(args),
            }) => {
                assert_eq!(args.board_type, "rk3568");
                assert!(args.server.server.is_none());
                assert!(args.server.port.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_board_connect_with_long_args() {
        let cli = Cli::try_parse_from([
            "ostool",
            "board",
            "connect",
            "--board-type",
            "rk3568",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            SubCommands::Board(BoardArgs {
                command: BoardSubCommands::Connect(args),
            }) => {
                assert_eq!(args.board_type, "rk3568");
                assert_eq!(args.server.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.server.port, Some(9000));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_board_connect_requires_board_type() {
        let err = Cli::try_parse_from(["ostool", "board", "connect"]).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("--board-type"));
    }

    #[test]
    fn parse_board_run_with_board_type() {
        let cli = Cli::try_parse_from(["ostool", "board", "run"]).unwrap();

        match cli.command {
            SubCommands::Board(BoardArgs {
                command: BoardSubCommands::Run(args),
            }) => {
                assert!(args.config.is_none());
                assert!(args.board_config.is_none());
                assert!(args.board_type.is_none());
                assert!(args.server.server.is_none());
                assert!(args.server.port.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_board_run_with_build_and_board_config() {
        let cli = Cli::try_parse_from([
            "ostool",
            "board",
            "run",
            "--config",
            "board.build.toml",
            "--board-config",
            "remote.board.toml",
            "--board-type",
            "rk3568",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            SubCommands::Board(BoardArgs {
                command: BoardSubCommands::Run(args),
            }) => {
                assert_eq!(
                    args.config.as_deref(),
                    Some(std::path::Path::new("board.build.toml"))
                );
                assert_eq!(
                    args.board_config.as_deref(),
                    Some(std::path::Path::new("remote.board.toml"))
                );
                assert_eq!(args.board_type.as_deref(), Some("rk3568"));
                assert_eq!(args.server.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.server.port, Some(9000));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn apply_cargo_selector_overrides_cargo_build_config() {
        let (_temp, mut tool) = test_tool();
        let mut build_config = build::config::BuildConfig {
            system: build::config::BuildSystem::Cargo(build::config::Cargo {
                package: "default-package".into(),
                bin: None,
                ..Default::default()
            }),
        };

        apply_cargo_selector(
            &mut tool,
            &mut build_config,
            &CargoSelectorArgs {
                package: Some("kernel".into()),
                bin: Some("kernel-qemu".into()),
            },
        )
        .unwrap();

        match &build_config.system {
            build::config::BuildSystem::Cargo(cargo) => {
                assert_eq!(cargo.package, "kernel");
                assert_eq!(cargo.bin.as_deref(), Some("kernel-qemu"));
            }
            other => panic!("unexpected build system: {other:?}"),
        }
        assert_eq!(tool.ctx().build_config.as_ref(), Some(&build_config));
    }

    #[test]
    fn apply_cargo_selector_rejects_custom_build_config() {
        let (_temp, mut tool) = test_tool();
        let mut build_config = build::config::BuildConfig {
            system: build::config::BuildSystem::Custom(build::config::Custom {
                build_cmd: "make".into(),
                elf_path: "target/kernel.elf".into(),
                to_bin: true,
            }),
        };

        let err = apply_cargo_selector(
            &mut tool,
            &mut build_config,
            &CargoSelectorArgs {
                package: Some("kernel".into()),
                bin: None,
            },
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("--package/--bin can only be used with system.Cargo")
        );
    }

    fn test_tool() -> (tempfile::TempDir, Tool) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"kernel\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "").unwrap();
        let tool = Tool::new(ToolConfig {
            manifest: Some(temp.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap();
        (temp, tool)
    }

    #[test]
    fn parse_board_config_command() {
        let cli = Cli::try_parse_from(["ostool", "board", "config"]).unwrap();

        match cli.command {
            SubCommands::Board(BoardArgs {
                command: BoardSubCommands::Config,
            }) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parse_run_board_is_rejected() {
        let err = Cli::try_parse_from(["ostool", "run", "board"]).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("unrecognized subcommand"));
        assert!(rendered.contains("board"));
    }
}
