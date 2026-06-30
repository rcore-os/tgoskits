use std::path::PathBuf;

use clap::{Args, Subcommand};

mod capture;
mod parser;
mod paths;
mod symbolize;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use capture::write_raw_blocks_from_output;
pub(crate) use capture::{
    BacktraceBlockCapture, BacktraceQemuCapture, flush_pending_stream_symbolize,
};
pub(crate) use paths::{arceos_rust_elf_path, std_test_elf_path};
pub(crate) use symbolize::{
    BacktraceSymbolizeSession, SymbolizeAfterQemuOutcome, keep_qemu_log_from_env,
    maybe_symbolize_after_qemu, symbolize_captured_blocks_to_string,
};
#[cfg(test)]
pub(crate) use symbolize::{
    apply_qemu_log_retention, should_delete_qemu_log_after_symbolize,
    should_persist_qemu_capture_log,
};

pub(super) const HOST_SYMBOLIZE_HEADER: &str = "=== host backtrace symbolize ===";

#[derive(Subcommand)]
pub enum Command {
    /// Extract and symbolize BACKTRACE_BEGIN/BT/BACKTRACE_END blocks from text logs.
    Symbolize(SymbolizeArgs),
}

#[derive(Args)]
pub struct SymbolizeArgs {
    /// Path to the kernel/app ELF file to symbolize addresses against.
    #[arg(long, value_name = "PATH")]
    pub elf: PathBuf,

    /// Path to the captured log. If omitted, read from stdin.
    #[arg(long, value_name = "PATH")]
    pub log: Option<PathBuf>,

    /// Only symbolize blocks whose kind matches this value.
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Subtract 1 from ip before symbolization (matches typical call-site adjustment).
    ///
    /// Use `--adjust-ip false` to disable.
    #[arg(
        long,
        value_name = "BOOL",
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = clap::value_parser!(bool)
    )]
    pub adjust_ip: bool,

    /// Apply a signed bias to ip before symbolization (useful when runtime addresses are slid).
    ///
    /// Example: `--ip-bias -0xffff_ffff_8000_0000`.
    #[arg(long, value_name = "I64", default_value_t = 0)]
    pub ip_bias: i64,
}

pub fn execute(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Symbolize(args) => symbolize::symbolize_cli(args),
    }
}
