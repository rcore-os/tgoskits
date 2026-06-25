use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run StarryOS QEMU test suite
    Qemu(ArgsTestQemu),
    /// Run StarryOS remote board test suite
    Board(ArgsTestBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list"],
        help = "StarryOS architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list"],
        help = "StarryOS target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one StarryOS QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(short = 'l', long, help = "List discovered StarryOS QEMU test cases")]
    pub list: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one Starry board test case"
    )]
    pub test_case: Option<String>,

    #[arg(
        long,
        value_name = "BOARD",
        help = "Run all Starry board test cases for one board"
    )]
    pub board: Option<String>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(short = 'l', long, help = "List discovered Starry board test cases")]
    pub list: bool,
}
