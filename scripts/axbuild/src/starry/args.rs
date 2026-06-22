use std::{fmt, path::PathBuf};

use clap::{Args, Subcommand, ValueEnum};

use super::{app, kmod, quick_start, rootfs, test};
use crate::context::StarryCliArgs;

/// StarryOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build StarryOS application
    Build(ArgsBuild),
    /// Build and run StarryOS application
    Qemu(ArgsQemu),
    /// Generate a default StarryOS board config
    Defconfig(ArgsDefconfig),
    /// StarryOS board config helpers
    Config(ArgsConfig),
    /// Build and profile StarryOS with qperf
    Perf(ArgsPerf),
    /// Run StarryOS test suites
    Test(test::ArgsTest),
    /// Run StarryOS runnable apps
    App(app::ArgsApp),
    /// Download rootfs image into workspace target directory
    Rootfs(rootfs::ArgsRootfs),
    /// Convenience entrypoints for common QEMU and Orange Pi workflows
    #[command(name = "quick-start")]
    QuickStart(quick_start::ArgsQuickStart),
    /// Build and run StarryOS application with U-Boot
    Uboot(ArgsUboot),
    /// Build and run StarryOS on a remote board
    Board(ArgsBoard),
    /// Build StarryOS loadable kernel modules (`.ko`)
    Kmod(kmod::ArgsKmod),
}

#[derive(Args, Clone)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,

    /// Override the rootfs disk image path (skips auto-download).
    #[arg(long, value_name = "IMAGE")]
    pub rootfs: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsPerf {
    /// Profile case name used in the default output path.
    #[arg(long, default_value = "boot")]
    pub case: String,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(long, default_value_t = 99)]
    pub freq: u32,
    #[arg(long = "out", hide = true)]
    pub out: Option<PathBuf>,
    /// Output root. Final reports go under <DIR>/perf/<arch>/latest.
    #[arg(long)]
    pub output_dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = PerfFormat::All)]
    pub format: PerfFormat,
    #[arg(long, default_value_t = 128)]
    pub max_depth: usize,
    #[arg(long, value_name = "SECONDS", default_value_t = 20)]
    pub timeout: u64,
    #[arg(long, value_enum, default_value_t = PerfMode::Tb)]
    pub mode: PerfMode,
    #[arg(long, default_value_t = 80)]
    pub top: usize,
    #[arg(long, default_value_t = 0.3)]
    pub min_percent: f64,
    #[arg(long)]
    pub debug: bool,
    #[arg(long)]
    pub kernel_filter: bool,
    /// Collect host wall/user/system CPU time metrics for the QEMU process wrapper.
    #[arg(long)]
    pub host_time: bool,
    /// Disable the cargo starry perf default host-time metrics.
    #[arg(long)]
    pub no_host_time: bool,
    /// Run QEMU under host perf stat. These are host/QEMU process metrics, not guest PMU values.
    #[arg(long)]
    pub host_perf: bool,
    /// Comma-separated host perf stat events used with --host-perf.
    #[arg(
        long,
        default_value = "task-clock,cycles,instructions,cache-references,cache-misses,\
                         context-switches,cpu-migrations,page-faults"
    )]
    pub host_perf_events: String,
    /// Send this command to the guest shell after the qperf boot prompt appears.
    #[arg(long, visible_alias = "workload")]
    pub shell_init_cmd: Option<String>,
    /// Prompt substring used before sending --shell-init-cmd.
    #[arg(long)]
    pub shell_prefix: Option<String>,
    /// Append one raw QEMU argument. Repeat for options and values.
    #[arg(long = "qemu-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub qemu_args: Vec<String>,
    /// Guest stdout marker that starts the workload sampling window.
    #[arg(long)]
    pub start_marker: Option<String>,
    /// Guest stdout marker that stops the workload sampling window.
    #[arg(long)]
    pub stop_marker: Option<String>,
    /// Stop QEMU if the workload window stays open longer than this many seconds.
    #[arg(long, value_name = "SECONDS")]
    pub workload_timeout: Option<u64>,
    /// Enable feature-gated in-guest qperf metric counters.
    #[arg(long)]
    pub qperf_metrics: bool,
    /// Request SVG flamegraph generation even when --format is folded.
    #[arg(long)]
    pub flamegraph: bool,
    /// Flamegraph view format.
    #[arg(long, value_enum, default_value_t = PerfFlamegraphKind::Svg)]
    pub flamegraph_kind: PerfFlamegraphKind,
    /// Preserve the deepest stack qperf can collect for this build.
    #[arg(long)]
    pub full_stack: bool,
    /// qperf callchain collection mode. `leaf` is fastest; `fp` requires frame pointers.
    #[arg(long = "perf-callchain", visible_alias = "callchain", value_enum)]
    pub callchain: Option<PerfCallchain>,
    /// Add DWARF debug info and keep symbols for qperf symbolization.
    #[arg(long = "perf-debuginfo")]
    pub debuginfo: bool,
    /// Force frame pointers for qperf FP unwinding.
    #[arg(long = "perf-force-frame-pointers")]
    pub force_frame_pointers: bool,
    /// Force Rust demangling in qperf-analyzer.
    #[arg(long)]
    pub demangle: bool,
    /// Keep tiny frames in SVG output by setting flamegraph min width to zero.
    #[arg(long)]
    pub no_truncate: bool,
    /// Include kernel symbols in symbolized stacks. This is the default for StarryOS kernels.
    #[arg(long)]
    pub include_kernel_symbols: bool,
    /// Include user symbols when available. Current StarryOS qperf only resolves the kernel ELF.
    #[arg(long)]
    pub include_user_symbols: bool,
    /// Folded-stack symbol style.
    #[arg(long, value_enum, default_value_t = PerfSymbolStyle::Full)]
    pub symbol_style: PerfSymbolStyle,
    /// Generate an additional focused folded stack/flamegraph for matching frames.
    #[arg(long, value_name = "REGEX")]
    pub focus: Option<String>,
    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PerfFormat {
    Folded,
    Svg,
    Pprof,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PerfMode {
    Tb,
    Insn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PerfCallchain {
    Leaf,
    Fp,
    Logical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PerfFlamegraphKind {
    Svg,
    Html,
    Folded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PerfSymbolStyle {
    Full,
    Short,
    Module,
}

impl fmt::Display for PerfMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tb => "tb",
            Self::Insn => "insn",
        })
    }
}

impl fmt::Display for PerfCallchain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Leaf => "leaf",
            Self::Fp => "fp",
            Self::Logical => "logical",
        })
    }
}

impl fmt::Display for PerfFlamegraphKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Svg => "svg",
            Self::Html => "html",
            Self::Folded => "folded",
        })
    }
}

impl fmt::Display for PerfSymbolStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full => "full",
            Self::Short => "short",
            Self::Module => "module",
        })
    }
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsBoard {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long)]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ArgsDefconfig {
    pub board: String,
}

#[derive(Args)]
pub struct ArgsConfig {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
}

impl From<&ArgsBuild> for StarryCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            smp: args.smp,
            debug: args.debug,
        }
    }
}
