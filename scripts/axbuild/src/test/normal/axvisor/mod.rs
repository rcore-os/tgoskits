mod assets;
mod board;
pub(crate) mod discovery;
mod host_probe;
mod http_probe;
mod initramfs;
mod ovmf;
mod qemu;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
use std::path::Path;

pub(crate) use types::{AxvisorQemuCase, BoardTestGroup};

use crate::axvisor::{ArgsTest, Axvisor, TestCommand};

pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    discovery::parse_target(arch, target)
}

#[cfg(test)]
fn discover_qemu_cases(
    workspace_root: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxvisorQemuCase>> {
    discovery::discover_qemu_cases(
        discovery::CaseSource::Tests,
        workspace_root,
        group,
        arch,
        target,
        selected_case,
    )
}

#[cfg(test)]
fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_cases: &[String],
    boards: &[String],
) -> anyhow::Result<Vec<BoardTestGroup>> {
    discovery::discover_board_test_groups(
        discovery::CaseSource::Tests,
        workspace_root,
        group,
        selected_cases,
        boards,
    )
}

pub(crate) async fn test(axvisor: &mut Axvisor, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => axvisor.test_qemu(args).await,
        TestCommand::Uboot(args) => axvisor.test_uboot(args).await,
        TestCommand::Board(args) => axvisor.test_board(args).await,
    }
}

const AXVISOR_TEST_SUITE_OS: &str = "axvisor";
const AXVISOR_NORMAL_GROUP: &str = "normal";
