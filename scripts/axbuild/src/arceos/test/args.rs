use anyhow::bail;
use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run ArceOS QEMU test suites (Rust + C by default)
    Qemu(ArgsTestQemu),
    /// Run ArceOS remote board test suite
    Board(ArgsTestBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(long, value_name = "ARCH", help = "ArceOS architecture to test")]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        help = "ArceOS target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run ArceOS QEMU test cases from one test group (rust or c)"
    )]
    pub test_group: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one ArceOS QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(short = 'l', long, help = "List discovered ArceOS QEMU test cases")]
    pub list: bool,
    /// Removed: Rust tests are selected with `--test-case`.
    #[arg(
        short,
        long,
        value_name = "PACKAGE",
        conflicts_with = "only_c",
        hide = true
    )]
    pub package: Vec<String>,
    /// Only run Rust tests; prefer `--test-group rust`
    #[arg(long, conflicts_with = "only_c", hide = true)]
    pub only_rust: bool,
    /// Only run C tests; prefer `--test-group c`
    #[arg(long, conflicts_with = "only_rust", hide = true)]
    pub only_c: bool,
    /// Skip host `backtrace symbolize` after each ArceOS **rust** QEMU case.
    #[arg(long = "no-symbolize", help_heading = "Backtrace")]
    pub no_symbolize: bool,
    /// Keep the QEMU backtrace capture log after successful host symbolize (default: delete).
    #[arg(long = "keep-qemu-log", help_heading = "Backtrace")]
    pub keep_qemu_log: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one ArceOS board test case"
    )]
    pub test_case: Option<String>,

    #[arg(
        long,
        value_name = "BOARD",
        help = "Run all ArceOS board test cases for one board"
    )]
    pub board: Option<String>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(short = 'l', long, help = "List discovered ArceOS board test cases")]
    pub list: bool,
}

pub(super) fn reject_removed_rust_package_filter(args: &ArgsTestQemu) -> anyhow::Result<()> {
    if args.package.is_empty() {
        return Ok(());
    }
    bail!(
        "ArceOS rust qemu tests no longer support --package; use --test-case <case> to select a \
         feature-gated test, or omit it to run the `all` feature in one QEMU boot"
    )
}

pub(super) fn reject_missing_qemu_target(args: &ArgsTestQemu) -> anyhow::Result<()> {
    if args.list || args.arch.is_some() || args.target.is_some() {
        return Ok(());
    }
    bail!("ArceOS qemu tests require --arch <ARCH> or --target <TARGET>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_qemu_run_still_requires_arch_or_target() {
        let args = ArgsTestQemu {
            arch: None,
            target: None,
            test_group: Some("rust".into()),
            test_case: None,
            list: false,
            package: Vec::new(),
            only_rust: false,
            only_c: false,
            no_symbolize: false,
            keep_qemu_log: false,
        };

        let err = reject_missing_qemu_target(&args).unwrap_err();
        assert!(err.to_string().contains("require --arch"));
    }
}
