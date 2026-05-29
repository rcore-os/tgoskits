use std::{
    fmt,
    path::{Path, PathBuf},
};

use clap::{Args, Subcommand, ValueEnum};
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::context::{AppContext, ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs};

pub(crate) mod apk;
pub mod app;
pub mod board;
pub mod build;
pub mod config;
pub mod perf;
pub mod quick_start;
pub(crate) mod resolver;
pub mod rootfs;
pub mod test;

/// StarryOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build StarryOS application
    Build(ArgsBuild),
    /// Build and run StarryOS application
    #[command(alias = "run")]
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

    #[command(flatten)]
    pub perf: ArgsQemuPerf,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsQemuPerf {
    /// Profile this run with qperf instead of launching a plain QEMU session.
    #[arg(long)]
    pub perf: bool,
    #[arg(long = "perf-case", value_name = "NAME")]
    pub case: Option<String>,
    #[arg(long = "perf-workload", value_name = "CMD")]
    pub workload: Option<String>,
    #[arg(long = "perf-shell-prefix", value_name = "PREFIX")]
    pub shell_prefix: Option<String>,
    #[arg(long = "perf-output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    #[arg(long = "perf-start-marker", value_name = "MARKER")]
    pub start_marker: Option<String>,
    #[arg(long = "perf-stop-marker", value_name = "MARKER")]
    pub stop_marker: Option<String>,
    #[arg(long = "perf-timeout", value_name = "SECONDS")]
    pub timeout: Option<u64>,
    #[arg(long = "perf-workload-timeout", value_name = "SECONDS")]
    pub workload_timeout: Option<u64>,
    #[arg(long = "perf-freq", value_name = "HZ")]
    pub freq: Option<u32>,
    #[arg(long = "perf-max-depth", value_name = "DEPTH")]
    pub max_depth: Option<usize>,
    #[arg(long = "perf-mode", value_enum)]
    pub mode: Option<PerfMode>,
    #[arg(long = "perf-format", value_enum)]
    pub format: Option<PerfFormat>,
    #[arg(long = "perf-top", value_name = "N")]
    pub top: Option<usize>,
    #[arg(long = "perf-min-percent", value_name = "PERCENT")]
    pub min_percent: Option<f64>,
    #[arg(long = "perf-host-time")]
    pub host_time: bool,
    #[arg(long = "perf-no-host-time")]
    pub no_host_time: bool,
    #[arg(long = "perf-host-perf")]
    pub host_perf: bool,
    #[arg(long = "perf-host-perf-events", value_name = "EVENTS")]
    pub host_perf_events: Option<String>,
    #[arg(long = "perf-qperf-metrics")]
    pub qperf_metrics: bool,
    #[arg(long = "perf-qemu-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub qemu_args: Vec<String>,
    #[arg(long = "perf-flamegraph")]
    pub flamegraph: bool,
    #[arg(long = "perf-flamegraph-kind", value_enum)]
    pub flamegraph_kind: Option<PerfFlamegraphKind>,
    #[arg(long = "perf-full-stack")]
    pub full_stack: bool,
    #[arg(long = "perf-demangle")]
    pub demangle: bool,
    #[arg(long = "perf-no-truncate")]
    pub no_truncate: bool,
    #[arg(long = "perf-symbol-style", value_enum)]
    pub symbol_style: Option<PerfSymbolStyle>,
    #[arg(long = "perf-focus", value_name = "REGEX")]
    pub focus: Option<String>,
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

pub struct Starry {
    pub(super) app: AppContext,
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

impl ArgsQemuPerf {
    fn has_overrides(&self) -> bool {
        self.case.is_some()
            || self.workload.is_some()
            || self.shell_prefix.is_some()
            || self.output_dir.is_some()
            || self.start_marker.is_some()
            || self.stop_marker.is_some()
            || self.timeout.is_some()
            || self.workload_timeout.is_some()
            || self.freq.is_some()
            || self.max_depth.is_some()
            || self.mode.is_some()
            || self.format.is_some()
            || self.top.is_some()
            || self.min_percent.is_some()
            || self.host_time
            || self.no_host_time
            || self.host_perf
            || self.host_perf_events.is_some()
            || self.qperf_metrics
            || !self.qemu_args.is_empty()
            || self.flamegraph
            || self.flamegraph_kind.is_some()
            || self.full_stack
            || self.demangle
            || self.no_truncate
            || self.symbol_style.is_some()
            || self.focus.is_some()
    }
}

fn perf_args_from_qemu(args: ArgsQemu) -> anyhow::Result<ArgsPerf> {
    if args.qemu_config.is_some() || args.rootfs.is_some() {
        anyhow::bail!(
            "cargo starry run --perf currently uses the default StarryOS qperf QEMU/rootfs flow; \
             --qemu-config and --rootfs are not supported with --perf yet"
        );
    }
    if args.build.config.is_some() || args.build.target.is_some() || args.build.smp.is_some() {
        anyhow::bail!(
            "cargo starry run --perf currently supports --arch and --debug build overrides; use \
             cargo starry perf for the default qperf path or plain cargo starry qemu for custom \
             --config/--target/--smp runs"
        );
    }
    let perf = args.perf;
    Ok(ArgsPerf {
        case: perf.case.unwrap_or_else(|| "boot".to_string()),
        arch: args.build.arch,
        freq: perf.freq.unwrap_or(99),
        out: None,
        output_dir: perf.output_dir,
        format: perf.format.unwrap_or(PerfFormat::All),
        max_depth: perf.max_depth.unwrap_or(128),
        timeout: perf.timeout.unwrap_or(20),
        mode: perf.mode.unwrap_or(PerfMode::Tb),
        top: perf.top.unwrap_or(80),
        min_percent: perf.min_percent.unwrap_or(0.3),
        debug: args.build.debug,
        kernel_filter: false,
        host_time: perf.host_time,
        no_host_time: perf.no_host_time,
        host_perf: perf.host_perf,
        host_perf_events: perf.host_perf_events.unwrap_or_else(|| {
            "task-clock,cycles,instructions,cache-references,cache-misses,context-switches,\
             cpu-migrations,page-faults"
                .to_string()
        }),
        shell_init_cmd: perf.workload,
        shell_prefix: perf.shell_prefix,
        qemu_args: perf.qemu_args,
        start_marker: perf.start_marker,
        stop_marker: perf.stop_marker,
        workload_timeout: perf.workload_timeout,
        qperf_metrics: perf.qperf_metrics,
        flamegraph: perf.flamegraph,
        flamegraph_kind: perf.flamegraph_kind.unwrap_or(PerfFlamegraphKind::Svg),
        full_stack: perf.full_stack,
        demangle: perf.demangle,
        no_truncate: perf.no_truncate,
        include_kernel_symbols: true,
        include_user_symbols: false,
        symbol_style: perf.symbol_style.unwrap_or(PerfSymbolStyle::Full),
        focus: perf.focus,
    })
}

impl Starry {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Defconfig(args) => self.defconfig(args),
            Command::Config(args) => self.config(args),
            Command::Perf(args) => self.perf(args).await,
            Command::Rootfs(args) => self.rootfs(args).await,
            Command::QuickStart(args) => self.quick_start(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Board(args) => self.board(args).await,
            Command::Test(args) => self.test(args).await,
            Command::App(args) => self.app_command(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        if args.perf.perf {
            return self.perf(perf_args_from_qemu(args)?).await;
        }
        if args.perf.has_overrides() {
            anyhow::bail!("--perf-* options require --perf");
        }
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        if let Some(board) = config::ensure_default_build_config_for_target(
            self.app.workspace_root(),
            &request.target,
            &request.build_info_path,
        )? {
            println!(
                "generated missing Starry qemu build config {} from board {}",
                request.build_info_path.display(),
                board.name
            );
        }
        if let Some(rootfs) = args.rootfs {
            rootfs::qemu_with_explicit_rootfs(self, request, rootfs).await
        } else {
            self.run_qemu_request(request).await
        }
    }

    async fn rootfs(&mut self, args: rootfs::ArgsRootfs) -> anyhow::Result<()> {
        rootfs::rootfs(self, args).await
    }

    async fn perf(&mut self, args: ArgsPerf) -> anyhow::Result<()> {
        perf::run(self, args).await
    }

    fn defconfig(&mut self, args: ArgsDefconfig) -> anyhow::Result<()> {
        let path = config::write_defconfig(self.app.workspace_root(), &args.board)?;
        println!("Generated {} for board {}", path.display(), args.board);
        Ok(())
    }

    fn config(&mut self, args: ArgsConfig) -> anyhow::Result<()> {
        match args.command {
            ConfigCommand::Ls => {
                for board in config::available_board_names(self.app.workspace_root())? {
                    println!("{board}");
                }
            }
        }
        Ok(())
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            None,
            args.uboot_config,
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    async fn board(&mut self, args: ArgsBoard) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args.build).into(), None, None, SnapshotPersistence::Store)?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let board_config = self
            .load_board_config(&cargo, args.board_config.as_deref())
            .await?;
        self.app
            .board(
                cargo,
                request.build_info_path,
                board_config,
                RunBoardOptions {
                    board_type: args.board_type,
                    server: args.server,
                    port: args.port,
                },
            )
            .await
    }

    async fn quick_start(&mut self, args: quick_start::ArgsQuickStart) -> anyhow::Result<()> {
        use quick_start::{
            QuickOrangeAction, QuickQemuPlatform, QuickSg2002Action, QuickStartCommand,
        };

        match args.command {
            QuickStartCommand::List => {
                quick_start::print_supported_platforms(self.app.workspace_root());
                Ok(())
            }
            QuickStartCommand::QemuAarch64(args) => {
                self.quick_start_qemu(QuickQemuPlatform::Aarch64, args.action)
                    .await
            }
            QuickStartCommand::QemuRiscv64(args) => {
                self.quick_start_qemu(QuickQemuPlatform::Riscv64, args.action)
                    .await
            }
            QuickStartCommand::QemuLoongarch64(args) => {
                self.quick_start_qemu(QuickQemuPlatform::Loongarch64, args.action)
                    .await
            }
            QuickStartCommand::QemuX8664(args) => {
                self.quick_start_qemu(QuickQemuPlatform::X8664, args.action)
                    .await
            }
            QuickStartCommand::Orangepi5Plus(args) => match args.action {
                QuickOrangeAction::Build(build_args) => {
                    self.quick_start_orangepi_build(build_args).await
                }
                QuickOrangeAction::Run(run_args) => self.quick_start_orangepi_run(run_args).await,
            },
            QuickStartCommand::LicheervNanoSg2002(args) => match args.action {
                QuickSg2002Action::Build => self.quick_start_sg2002_build().await,
                QuickSg2002Action::Run(run_args) => self.quick_start_sg2002_run(run_args).await,
            },
        }
    }
    async fn test(&mut self, args: test::ArgsTest) -> anyhow::Result<()> {
        test::test(self, args).await
    }

    async fn app_command(&mut self, args: app::ArgsApp) -> anyhow::Result<()> {
        match args.command {
            app::AppCommand::List(args) => app::print_apps(self.app.workspace_root(), args.kind),
            app::AppCommand::Run(args) => self.app_run(args).await,
            app::AppCommand::Board(args) => self.app_board(args).await,
        }
    }

    async fn app_run(&mut self, args: app::ArgsAppRun) -> anyhow::Result<()> {
        let apps = app::selected_apps(self.app.workspace_root(), &args)?;
        for app in apps {
            let missing = app::missing_caps(&app, &args.caps);
            if !missing.is_empty() {
                if args.test_case.is_some() {
                    anyhow::bail!(
                        "Starry app `{}` is missing required capabilities: {}",
                        app.name,
                        missing.join(", ")
                    );
                }
                println!("SKIP	{}	missing {}", app.name, missing.join(","));
                continue;
            }

            match app.kind {
                app::StarryAppKind::Qemu => self.app_qemu(&app, &args).await?,
                app::StarryAppKind::Board => {
                    let board_args = app::ArgsAppBoard {
                        test_case: app.name.clone(),
                        board_config: None,
                        board_type: None,
                        server: None,
                        port: None,
                        debug: args.debug,
                    };
                    self.app_board(board_args).await?;
                }
            }
        }
        Ok(())
    }

    async fn app_qemu(
        &mut self,
        app: &app::StarryAppCase,
        args: &app::ArgsAppRun,
    ) -> anyhow::Result<()> {
        let case = app::prepare_qemu_app_case(
            self.app.workspace_root(),
            app,
            args.arch.as_deref(),
            args.qemu_config.as_deref(),
        )
        .await?;
        let request = self.prepare_request(
            StarryCliArgs {
                config: case.build_config_path.clone(),
                arch: Some(case.arch.clone()),
                target: Some(case.target.clone()),
                smp: None,
                debug: args.debug,
            },
            case.qemu_config_path.clone(),
            None,
            SnapshotPersistence::Store,
        )?;
        rootfs::qemu_with_explicit_rootfs(self, request, case.rootfs_path).await
    }

    async fn app_board(&mut self, args: app::ArgsAppBoard) -> anyhow::Result<()> {
        let case = app::resolve_board_case(
            self.app.workspace_root(),
            &args.test_case,
            args.board_config.as_deref(),
        )?;
        let request = self.prepare_request(
            StarryCliArgs {
                config: Some(case.build_config_path.clone()),
                arch: None,
                target: Some(case.target.clone()),
                smp: None,
                debug: args.debug,
            },
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let mut board_config = self
            .load_board_config(&cargo, Some(case.board_config_path.as_path()))
            .await?;
        board_config.shell_init_cmd = Some(case.init_cmd);
        self.app
            .board(
                cargo,
                request.build_info_path,
                board_config,
                RunBoardOptions {
                    board_type: args.board_type,
                    server: args.server,
                    port: args.port,
                },
            )
            .await
    }

    pub(super) fn prepare_request(
        &self,
        args: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        let (request, snapshot) = self.app.prepare_starry_request(
            args,
            qemu_config,
            uboot_config,
            build::resolve_build_info_path,
        )?;
        if persistence.should_store() {
            self.app.store_starry_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn quick_start_build_args(arch: &str, config: PathBuf) -> StarryCliArgs {
        StarryCliArgs {
            config: Some(config),
            arch: Some(arch.to_string()),
            target: None,
            smp: None,
            debug: false,
        }
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
                .read_uboot_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn load_board_config(
        &mut self,
        cargo: &Cargo,
        board_config_path: Option<&Path>,
    ) -> anyhow::Result<BoardRunConfig> {
        match board_config_path {
            Some(path) => {
                self.app
                    .tool_mut()
                    .read_board_run_config_from_path_for_cargo(cargo, path)
                    .await
            }
            None => {
                let workspace_root = self.app.workspace_root().to_path_buf();
                self.app
                    .tool_mut()
                    .ensure_board_run_config_in_dir_for_cargo(cargo, &workspace_root)
                    .await
            }
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        rootfs::qemu(self, request).await
    }

    async fn run_build_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app.uboot(cargo, request.build_info_path, uboot).await
    }

    async fn quick_start_qemu(
        &mut self,
        platform: quick_start::QuickQemuPlatform,
        action: quick_start::QuickQemuAction,
    ) -> anyhow::Result<()> {
        let arch = platform.arch();

        match action {
            quick_start::QuickQemuAction::Build => {
                quick_start::refresh_qemu_build_config(self.app.workspace_root(), platform)?;
                rootfs::ensure_quick_start_qemu_rootfs(self.app.workspace_root(), arch).await?;
                let request = self.prepare_request(
                    Self::quick_start_build_args(
                        arch,
                        quick_start::tmp_qemu_build_config_path(
                            self.app.workspace_root(),
                            platform,
                        ),
                    ),
                    None,
                    None,
                    SnapshotPersistence::Store,
                )?;
                self.run_build_request(request).await
            }
            quick_start::QuickQemuAction::Run => {
                quick_start::ensure_qemu_build_config(self.app.workspace_root(), platform)?;
                let request = self.prepare_request(
                    Self::quick_start_build_args(
                        arch,
                        quick_start::tmp_qemu_build_config_path(
                            self.app.workspace_root(),
                            platform,
                        ),
                    ),
                    None,
                    None,
                    SnapshotPersistence::Store,
                )?;
                self.run_qemu_request(request).await
            }
        }
    }

    async fn quick_start_orangepi_build(
        &mut self,
        args: quick_start::QuickOrangeConfigArgs,
    ) -> anyhow::Result<()> {
        quick_start::prepare_orangepi_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_orangepi_configs(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "aarch64",
                quick_start::tmp_orangepi_build_config_path(self.app.workspace_root()),
            ),
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.run_build_request(request).await
    }

    async fn quick_start_orangepi_run(
        &mut self,
        args: quick_start::QuickOrangeRunArgs,
    ) -> anyhow::Result<()> {
        let uboot_config =
            quick_start::prepare_orangepi_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_orangepi_configs(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "aarch64",
                quick_start::tmp_orangepi_build_config_path(self.app.workspace_root()),
            ),
            None,
            Some(uboot_config),
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    async fn quick_start_sg2002_build(&mut self) -> anyhow::Result<()> {
        quick_start::refresh_sg2002_config(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "riscv64",
                quick_start::tmp_sg2002_build_config_path(self.app.workspace_root()),
            ),
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.run_build_request(request).await
    }

    async fn quick_start_sg2002_run(
        &mut self,
        args: quick_start::QuickSg2002RunArgs,
    ) -> anyhow::Result<()> {
        let uboot_config_path =
            quick_start::prepare_sg2002_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_sg2002_config(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "riscv64",
                quick_start::tmp_sg2002_build_config_path(self.app.workspace_root()),
            ),
            None,
            Some(uboot_config_path),
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }
}

pub(crate) fn default_qemu_config_template_path(workspace_root: &Path, arch: &str) -> PathBuf {
    workspace_root.join(format!("os/StarryOS/configs/qemu/qemu-{arch}.toml"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::starry::test::TestCommand;

    #[test]
    fn command_parses_test_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "test", "qemu", "--target", "x86_64"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target.as_deref(), Some("x86_64"));
                    assert_eq!(args.test_group, None);
                    assert!(!args.stress);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_defconfig() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "defconfig", "qemu-aarch64"]).unwrap();

        match cli.command {
            Command::Defconfig(args) => assert_eq!(args.board, "qemu-aarch64"),
            _ => panic!("expected defconfig command"),
        }
    }

    #[test]
    fn command_parses_config_ls() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "config", "ls"]).unwrap();

        match cli.command {
            Command::Config(args) => match args.command {
                ConfigCommand::Ls => {}
            },
            _ => panic!("expected config ls command"),
        }
    }

    #[test]
    fn command_parses_perf_workload_options() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "perf",
            "--arch",
            "riscv64",
            "--shell-init-cmd",
            "echo qperf",
            "--shell-prefix",
            "root@starry:",
            "--host-time",
            "--host-perf",
            "--host-perf-events",
            "task-clock,instructions",
            "--qemu-arg=-device",
            "--qemu-arg=vhost-vsock-pci,guest-cid=3",
            "--start-marker",
            "QPERF_BEGIN",
            "--stop-marker",
            "QPERF_END",
            "--workload-timeout",
            "5",
            "--qperf-metrics",
        ])
        .unwrap();

        match cli.command {
            Command::Perf(args) => {
                assert_eq!(args.arch.as_deref(), Some("riscv64"));
                assert_eq!(args.shell_init_cmd.as_deref(), Some("echo qperf"));
                assert_eq!(args.shell_prefix.as_deref(), Some("root@starry:"));
                assert!(args.host_time);
                assert!(args.host_perf);
                assert_eq!(args.host_perf_events, "task-clock,instructions");
                assert_eq!(
                    args.qemu_args,
                    vec!["-device", "vhost-vsock-pci,guest-cid=3"]
                );
                assert_eq!(args.start_marker.as_deref(), Some("QPERF_BEGIN"));
                assert_eq!(args.stop_marker.as_deref(), Some("QPERF_END"));
                assert_eq!(args.workload_timeout, Some(5));
                assert!(args.qperf_metrics);
            }
            _ => panic!("expected perf command"),
        }
    }

    #[test]
    fn command_parses_perf_flamegraph_options() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "perf",
            "--case",
            "blk-read",
            "--workload",
            "echo qperf",
            "--flamegraph-kind",
            "html",
            "--full-stack",
            "--no-truncate",
            "--symbol-style",
            "module",
            "--focus",
            "virtio|VirtQueue",
            "--min-percent",
            "0",
        ])
        .unwrap();

        match cli.command {
            Command::Perf(args) => {
                assert_eq!(args.case, "blk-read");
                assert_eq!(args.shell_init_cmd.as_deref(), Some("echo qperf"));
                assert_eq!(args.flamegraph_kind, PerfFlamegraphKind::Html);
                assert!(args.full_stack);
                assert!(args.no_truncate);
                assert_eq!(args.symbol_style, PerfSymbolStyle::Module);
                assert_eq!(args.focus.as_deref(), Some("virtio|VirtQueue"));
                assert_eq!(args.min_percent, 0.0);
            }
            _ => panic!("expected perf command"),
        }
    }

    #[test]
    fn command_parses_run_perf_alias() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "run",
            "--arch",
            "riscv64",
            "--perf",
            "--perf-case",
            "net-wget",
            "--perf-workload",
            "echo qperf",
            "--perf-qperf-metrics",
            "--perf-symbol-style",
            "short",
            "--perf-focus",
            "memcpy|memmove",
        ])
        .unwrap();

        match cli.command {
            Command::Qemu(args) => {
                assert_eq!(args.build.arch.as_deref(), Some("riscv64"));
                assert!(args.perf.perf);
                assert_eq!(args.perf.case.as_deref(), Some("net-wget"));
                assert_eq!(args.perf.workload.as_deref(), Some("echo qperf"));
                assert!(args.perf.qperf_metrics);
                assert_eq!(args.perf.symbol_style, Some(PerfSymbolStyle::Short));
                assert_eq!(args.perf.focus.as_deref(), Some("memcpy|memmove"));
            }
            _ => panic!("expected qemu command"),
        }
    }

    #[test]
    fn command_parses_test_board() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "test",
            "board",
            "-g",
            "normal",
            "-c",
            "smoke",
            "--board",
            "orangepi-5-plus",
            "-b",
            "OrangePi-5-Plus",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Board(args) => {
                    assert_eq!(args.test_group.as_deref(), Some("normal"));
                    assert_eq!(args.test_case.as_deref(), Some("smoke"));
                    assert_eq!(args.board.as_deref(), Some("orangepi-5-plus"));
                    assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                }
                _ => panic!("expected board test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_with_group_and_case() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry", "test", "qemu", "--arch", "x86_64", "-g", "stress", "-c", "smoke",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.target, None);
                    assert_eq!(args.test_group.as_deref(), Some("stress"));
                    assert_eq!(args.test_case, Some("smoke".to_string()));
                    assert!(!args.stress);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_with_stress_alias() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "test", "qemu", "--arch", "x86_64", "--stress"])
            .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.test_group, None);
                    assert!(args.stress);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_with_target() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli =
            Cli::try_parse_from(["starry", "test", "qemu", "--target", "x86_64-unknown-none"])
                .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_rejects_removed_shell_init_cmd_flag() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(
            Cli::try_parse_from([
                "starry",
                "test",
                "qemu",
                "--target",
                "x86_64",
                "--shell-init-cmd",
                "echo test",
            ])
            .is_err()
        );
    }

    #[test]
    fn command_rejects_removed_timeout_flag() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(
            Cli::try_parse_from([
                "starry",
                "test",
                "qemu",
                "--target",
                "x86_64",
                "--timeout",
                "10",
            ])
            .is_err()
        );
    }

    #[test]
    fn command_parses_quick_start_qemu_build() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "quick-start", "qemu-aarch64", "build"]).unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::QemuAarch64(inner) => {
                    assert!(matches!(inner.action, quick_start::QuickQemuAction::Build));
                }
                _ => panic!("expected qemu-aarch64 quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_rejects_removed_plat_dyn_flag() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(Cli::try_parse_from(["starry", "qemu", "--plat-dyn"]).is_err());
    }

    #[test]
    fn command_parses_quick_start_orangepi_run() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "quick-start",
            "orangepi-5-plus",
            "run",
            "--serial",
            "/dev/ttyUSB0",
            "--baud",
            "1500000",
        ])
        .unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::Orangepi5Plus(inner) => match inner.action {
                    quick_start::QuickOrangeAction::Run(run) => {
                        assert_eq!(run.serial.as_deref(), Some("/dev/ttyUSB0"));
                        assert_eq!(run.baud.as_deref(), Some("1500000"));
                    }
                    _ => panic!("expected orangepi run quick-start command"),
                },
                _ => panic!("expected orangepi quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_parses_quick_start_orangepi_build_with_overrides() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "quick-start",
            "orangepi-5-plus",
            "build",
            "--serial",
            "/dev/ttyUSB0",
        ])
        .unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::Orangepi5Plus(inner) => match inner.action {
                    quick_start::QuickOrangeAction::Build(build) => {
                        assert_eq!(build.serial.as_deref(), Some("/dev/ttyUSB0"));
                    }
                    _ => panic!("expected orangepi build quick-start command"),
                },
                _ => panic!("expected orangepi quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_parses_quick_start_sg2002_local_run() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "quick-start",
            "licheerv-nano-sg2002",
            "run",
            "--serial",
            "/dev/ttyUSB1",
            "--baud",
            "115200",
        ])
        .unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::LicheervNanoSg2002(inner) => match inner.action {
                    quick_start::QuickSg2002Action::Run(run) => {
                        assert_eq!(run.serial.as_deref(), Some("/dev/ttyUSB1"));
                        assert_eq!(run.baud.as_deref(), Some("115200"));
                    }
                    _ => panic!("expected sg2002 run quick-start command"),
                },
                _ => panic!("expected sg2002 quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_parses_app_board() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "app",
            "board",
            "-t",
            "orangepi-5-plus-uvc",
            "-b",
            "OrangePi-5-Plus",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
            "--debug",
        ])
        .unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::Board(args) => {
                    assert_eq!(args.test_case, "orangepi-5-plus-uvc");
                    assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                    assert!(args.debug);
                }
                _ => panic!("expected app board command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_parses_app_board_with_long_case_and_config() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "app",
            "board",
            "--test-case",
            "orangepi-5-plus-uvc",
            "--board-config",
            "board.toml",
        ])
        .unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::Board(args) => {
                    assert_eq!(args.test_case, "orangepi-5-plus-uvc");
                    assert_eq!(args.board_config, Some(PathBuf::from("board.toml")));
                }
                _ => panic!("expected app board command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_parses_app_list() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "app", "list", "--kind", "qemu"]).unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::List(args) => {
                    assert_eq!(args.kind, Some(app::StarryAppKind::Qemu))
                }
                _ => panic!("expected app list command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_parses_app_run_all_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "app",
            "run",
            "--all",
            "--kind",
            "qemu",
            "--cap",
            "board:OrangePi-5-Plus",
            "--arch",
            "x86_64",
            "--qemu-config",
            "qemu.toml",
            "--debug",
        ])
        .unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::Run(args) => {
                    assert!(args.all);
                    assert_eq!(args.kind, Some(app::StarryAppKind::Qemu));
                    assert_eq!(args.caps, vec!["board:OrangePi-5-Plus"]);
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.qemu_config, Some(PathBuf::from("qemu.toml")));
                    assert!(args.debug);
                }
                _ => panic!("expected app run command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_rejects_app_board_without_case() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(Cli::try_parse_from(["starry", "app", "board"]).is_err());
    }

    #[test]
    fn command_parses_board() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "board",
            "--arch",
            "aarch64",
            "--board-config",
            "remote.board.toml",
            "-b",
            "rk3568",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Command::Board(args) => {
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.board_config, Some(PathBuf::from("remote.board.toml")));
                assert_eq!(args.board_type.as_deref(), Some("rk3568"));
                assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.port, Some(9000));
            }
            _ => panic!("expected board command"),
        }
    }
}
