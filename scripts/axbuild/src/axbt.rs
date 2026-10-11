use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AxbtArgs {
    /// Final ELF from which the target-side map is generated.
    #[arg(long)]
    elf: PathBuf,
    /// Output path; defaults to the ELF path with an `.axbt` extension.
    #[arg(long)]
    output: Option<PathBuf>,
}

pub(crate) fn run(args: AxbtArgs) -> Result<()> {
    let output = args
        .output
        .unwrap_or_else(|| args.elf.with_extension("axbt"));
    crate::build::symbol_map::generate_axbt_map(&args.elf, &output)
}
