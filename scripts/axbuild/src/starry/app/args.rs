use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::StarryAppKind;

#[derive(Args, Debug, Clone)]
pub struct ArgsApp {
    #[command(subcommand)]
    pub command: AppCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AppCommand {
    /// List discovered StarryOS apps
    List(ArgsAppList),
    /// Build and run a StarryOS QEMU app
    Qemu(ArgsAppQemu),
    /// Build and run a StarryOS app on a remote board
    Board(ArgsAppBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAppList {
    #[arg(long, value_enum)]
    pub kind: Option<StarryAppKind>,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAppQemu {
    /// Run all discovered QEMU apps after capability filtering
    #[arg(long)]
    pub all: bool,

    /// Select `apps/starry/<CASE>`.
    #[arg(short = 't', long = "test-case", value_name = "CASE")]
    pub test_case: Option<String>,

    /// Declare an available capability, e.g. board:OrangePi-5-Plus
    #[arg(long = "cap", value_name = "CAP")]
    pub caps: Vec<String>,

    #[arg(long)]
    pub arch: Option<String>,

    #[arg(long = "qemu-config")]
    pub qemu_config: Option<PathBuf>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAppBoard {
    /// Select `apps/starry/<CASE>`.
    #[arg(short = 't', long = "test-case", value_name = "CASE")]
    pub test_case: String,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long)]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(long)]
    pub debug: bool,
}
