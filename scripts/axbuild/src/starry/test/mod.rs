mod args;
mod assets;
mod board;
mod qemu_discovery;
mod qemu_run;
mod suite;
mod symbolize;
mod types;

#[cfg(test)]
mod tests;

pub use args::{ArgsTest, ArgsTestBoard, ArgsTestQemu, TestCommand};
pub(crate) use assets::starry_case_asset_config;
pub(crate) use board::collect_board_test_groups;
pub(crate) use qemu_discovery::{
    direct_starry_qemu_case_exists, discover_qemu_cases, parse_starry_qemu_case_selection,
    parse_test_target,
};
#[cfg(test)]
pub(crate) use suite::render_qemu_case_summary;
pub(crate) use suite::{
    discover_all_qemu_cases_with_archs, discover_board_test_groups, finalize_qemu_case_run,
    require_test_suite_dir,
};
pub(crate) use symbolize::{
    ensure_host_symbolize_output_matches, start_qemu_case_host_http_server,
};
pub(crate) use types::{
    PreparedStarryQemuCase, StarryBoardTestGroup, StarryQemuCase, StarryQemuCaseOutcome,
    StarryQemuCaseReport, StarryQemuCaseRequirements, StarryQemuRunReport,
};

use super::Starry;

pub(super) async fn test(starry: &mut Starry, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => starry.test_qemu(args).await,
        TestCommand::Board(args) => starry.test_board(args).await,
    }
}

pub(crate) const STARRY_TEST_SUITE_OS: &str = "starryos";
