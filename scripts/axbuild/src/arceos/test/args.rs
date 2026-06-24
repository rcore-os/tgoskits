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
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list"],
        help = "ArceOS architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list"],
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

pub(super) fn reject_removed_rust_package_filter(args: &ArgsTestQemu) -> anyhow::Result<()> {
    if args.package.is_empty() {
        return Ok(());
    }
    bail!(
        "ArceOS rust qemu tests no longer support --package; use --test-case <case> to select a \
         feature-gated test, or omit it to run the `all` feature in one QEMU boot"
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::arceos::Command;

    #[test]
    fn command_parses_test_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli =
            Cli::try_parse_from(["arceos", "test", "qemu", "--target", "x86_64-unknown-none"])
                .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
                    assert!(!args.only_rust);
                    assert!(!args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_only_rust() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-rust",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
                    assert!(args.only_rust);
                    assert!(!args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_only_c() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-c",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
                    assert!(!args.only_rust);
                    assert!(args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_rejects_both_only_flags() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let result = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-rust",
            "--only-c",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn command_parses_removed_test_qemu_package_filter() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "riscv64gc-unknown-none-elf",
            "--package",
            "arceos-test-suit",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("riscv64gc-unknown-none-elf"));
                    assert_eq!(args.package, vec!["arceos-test-suit".to_string()]);
                    let err = reject_removed_rust_package_filter(&args).unwrap_err();
                    assert!(err.to_string().contains("no longer support --package"));
                    assert!(!args.only_rust);
                    assert!(!args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }
}
