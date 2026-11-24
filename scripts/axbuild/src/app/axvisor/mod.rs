//! Select applications independently from test suites and reuse the case runners.
use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::axvisor::{ArgsTestBoard, ArgsTestQemu, Axvisor, test::discovery::CaseSource};

#[derive(Args)]
pub struct ArgsApp {
    #[command(subcommand)]
    pub command: AppCommand,
}

#[derive(Subcommand)]
pub enum AppCommand {
    /// List available AxVisor board and QEMU applications
    List,
    /// Build and run an application on a remote board
    Board(ArgsAppBoard),
    /// Build and run an application in QEMU
    Qemu(ArgsAppQemu),
}

#[derive(Args, Default)]
pub struct AppSelection {
    /// Explicit application root containing groups; defaults to the workspace app root
    #[arg(long, value_name = "PATH")]
    pub app_root: Option<PathBuf>,
    /// Run selected checks within each selected application
    #[arg(long = "test-case", value_delimiter = ',')]
    pub test_cases: Vec<String>,
    /// Application directory relative to the selected group; includes descendants
    #[arg(
        short = 't',
        long = "app",
        value_name = "PATH",
        value_delimiter = ',',
        required_unless_present = "list"
    )]
    pub apps: Vec<String>,
    #[arg(short = 'g', long = "group", value_name = "GROUP")]
    pub group: Option<String>,
    /// List matching configurations without preparing or running them
    #[arg(short = 'l', long)]
    pub list: bool,
}

#[derive(Args)]
pub struct ArgsAppBoard {
    #[command(flatten)]
    pub selection: AppSelection,
    /// Select a board configuration when an application provides several
    #[arg(long, value_name = "BOARD", value_delimiter = ',')]
    pub board: Vec<String>,
    #[arg(short = 'b', long = "board-type")]
    pub board_type: Option<String>,
    #[arg(long)]
    pub server: Option<String>,
    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ArgsAppQemu {
    #[command(flatten)]
    pub selection: AppSelection,
    #[arg(long, required_unless_present_any = ["target", "list"])]
    pub arch: Option<String>,
    #[arg(long, required_unless_present_any = ["arch", "list"])]
    pub target: Option<String>,
}

impl Axvisor {
    pub(crate) async fn app_command(&mut self, args: ArgsApp) -> anyhow::Result<()> {
        match args.command {
            AppCommand::List => {
                println!("AxVisor board applications:");
                self.run_board_cases(
                    ArgsTestBoard {
                        test_root: None,
                        list: true,
                        ..Default::default()
                    },
                    CaseSource::Apps,
                )
                .await?;
                println!("AxVisor QEMU applications:");
                self.run_qemu_cases(
                    ArgsTestQemu {
                        test_root: None,
                        arch: None,
                        target: None,
                        test_group: None,
                        test_case: Vec::new(),
                        list: true,
                    },
                    CaseSource::Apps,
                )
                .await
            }
            AppCommand::Board(args) => {
                let selectors = app_member_selectors(&args.selection);
                let root = args
                    .selection
                    .app_root
                    .as_ref()
                    .map(|path| path.canonicalize())
                    .transpose()?;
                let source = root
                    .as_deref()
                    .map_or(CaseSource::Apps, CaseSource::ExternalApps);
                self.run_board_cases(
                    ArgsTestBoard {
                        test_root: None,
                        test_group: args.selection.group,
                        test_case: selectors,
                        board: args.board,
                        board_type: args.board_type,
                        server: args.server,
                        port: args.port,
                        list: args.selection.list,
                    },
                    source,
                )
                .await
            }
            AppCommand::Qemu(args) => {
                let selectors = app_member_selectors(&args.selection);
                let root = args
                    .selection
                    .app_root
                    .as_ref()
                    .map(|path| path.canonicalize())
                    .transpose()?;
                let source = root
                    .as_deref()
                    .map_or(CaseSource::Apps, CaseSource::ExternalApps);
                self.run_qemu_cases(
                    ArgsTestQemu {
                        test_root: None,
                        arch: args.arch,
                        target: args.target,
                        test_group: args.selection.group,
                        test_case: selectors,
                        list: args.selection.list,
                    },
                    source,
                )
                .await
            }
        }
    }
}

fn app_member_selectors(selection: &AppSelection) -> Vec<String> {
    if selection.apps.is_empty() {
        return selection.test_cases.clone();
    }
    if selection.test_cases.is_empty() {
        return selection.apps.clone();
    }
    selection
        .apps
        .iter()
        .flat_map(|app| {
            selection
                .test_cases
                .iter()
                .map(move |case| format!("{app}/{case}"))
        })
        .collect()
}
