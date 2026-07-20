mod assets;
mod board;
mod discovery;
mod qemu;
mod types;

#[cfg(test)]
mod tests;

use std::path::Path;

pub(crate) use types::{AxvisorQemuCase, BoardTestGroup};

use super::{ArgsTest, Axvisor, TestCommand};

pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    discovery::parse_target(arch, target)
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxvisorQemuCase>> {
    discovery::discover_qemu_cases(workspace_root, group, arch, target, selected_case)
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    board: Option<&str>,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    discovery::discover_board_test_groups(workspace_root, group, selected_case, board)
}

pub(super) async fn test(axvisor: &mut Axvisor, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => axvisor.test_qemu(args).await,
        TestCommand::Uboot(args) => axvisor.test_uboot(args).await,
        TestCommand::Board(args) => axvisor.test_board(args).await,
    }
}

const AXVISOR_TEST_SUITE_OS: &str = "axvisor";
const AXVISOR_NORMAL_GROUP: &str = "normal";
