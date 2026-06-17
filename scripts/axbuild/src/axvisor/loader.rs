use std::{path::Path, process::Command};

use clap::Args;

use crate::support::process::ProcessExt;

const AXLOADER_PACKAGE: &str = "axloader";
const AXLOADER_BIN: &str = "axloader";
const DEFAULT_UEFI_TARGET: &str = "x86_64-unknown-uefi";

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsBuild {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,

    #[arg(long, conflicts_with = "debug")]
    pub release: bool,

    #[arg(long, conflicts_with = "release")]
    pub debug: bool,
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct ArgsTest {
    #[arg(long, default_value = DEFAULT_UEFI_TARGET)]
    pub target: String,
}

pub fn build(workspace_root: &Path, args: ArgsBuild) -> anyhow::Result<()> {
    run_loader_build(workspace_root, &args.target, args.release || !args.debug)
}

pub fn test(workspace_root: &Path, args: ArgsTest) -> anyhow::Result<()> {
    run_cargo(
        workspace_root,
        ["test", "-p", AXLOADER_PACKAGE, "--all-targets"],
    )?;
    run_cargo(
        workspace_root,
        [
            "check",
            "-p",
            AXLOADER_PACKAGE,
            "--target",
            args.target.as_str(),
            "--bin",
            AXLOADER_BIN,
        ],
    )
}

fn run_loader_build(workspace_root: &Path, target: &str, release: bool) -> anyhow::Result<()> {
    let mut args = vec![
        "build",
        "-p",
        AXLOADER_PACKAGE,
        "--target",
        target,
        "--bin",
        AXLOADER_BIN,
    ];
    if release {
        args.push("--release");
    }
    run_cargo(workspace_root, args)
}

fn run_cargo<'a>(
    workspace_root: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> anyhow::Result<()> {
    let mut command = Command::new("cargo");
    command.current_dir(workspace_root).args(args);
    command.exec()
}
