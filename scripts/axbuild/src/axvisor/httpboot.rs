use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, anyhow, bail};
use clap::{Args as ClapArgs, Subcommand};
use object::{
    Object, ObjectSymbol,
    read::elf::{FileHeader, ProgramHeader},
};
use ostool::board::terminal;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    axvisor::{ArgsBuild, Axvisor},
    backtrace,
    context::{SnapshotPersistence, axbuild_tmp_dir},
};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub publish: ArgsPublish,
}

#[derive(Subcommand)]
pub enum Command {
    /// Check whether the built Axvisor ELF matches the bare-bin HTTP Boot shape.
    Check(ArgsCheck),
    /// Generate a minimal .httpboot.toml for local publishing.
    Init(ArgsInit),
    /// Build/pack Axvisor and publish HTTP Boot artifacts to ostool-server.
    Publish(ArgsPublish),
}

#[derive(ClapArgs)]
pub struct ArgsCheck {
    #[command(flatten)]
    pub build: ArgsBuild,

    /// Inspect an existing ELF instead of building first.
    #[arg(long, value_name = "PATH")]
    pub elf: Option<PathBuf>,
}

#[derive(ClapArgs)]
pub struct ArgsInit {
    /// Board id/type to lease from ostool-server.
    #[arg(
        short = 'b',
        long = "board",
        alias = "board-type",
        value_name = "BOARD"
    )]
    pub board: String,

    /// Output config path.
    #[arg(short, long, value_name = "PATH", default_value = ".httpboot.toml")]
    pub output: PathBuf,

    /// Overwrite an existing config file.
    #[arg(long)]
    pub force: bool,

    /// Do not open the serial console after publishing.
    #[arg(long = "no-open-console")]
    pub no_open_console: bool,
}

#[derive(ClapArgs)]
pub struct ArgsPublish {
    #[command(flatten)]
    pub build: ArgsBuild,

    /// Read HTTP Boot publish defaults from a TOML file. Defaults to .httpboot.toml.
    #[arg(long = "httpboot-config", value_name = "PATH")]
    pub httpboot_config: Option<PathBuf>,

    /// Build output ELF to publish; skips the build step when set.
    #[arg(long, value_name = "PATH")]
    pub elf: Option<PathBuf>,

    /// Output path for the generated flat kernel image.
    #[arg(long = "kernel-bin", value_name = "PATH")]
    pub kernel_bin: Option<PathBuf>,

    /// ostool-server board type/name to lease.
    #[arg(
        short = 'b',
        long = "board",
        alias = "board-type",
        value_name = "BOARD"
    )]
    pub board_type: Option<String>,

    /// ostool-server host.
    #[arg(long)]
    pub server: Option<String>,

    /// ostool-server port.
    #[arg(long)]
    pub port: Option<u16>,

    /// Remote kernel filename under the board current HTTP Boot directory.
    #[arg(long = "remote-name", value_name = "NAME")]
    pub remote_name: Option<String>,

    /// Override manifest.kernel_load_addr.
    #[arg(long = "kernel-load-addr", value_name = "HEX")]
    pub kernel_load_addr: Option<String>,

    /// Override manifest.entry_point.
    #[arg(long = "entry-point", value_name = "HEX")]
    pub entry_point: Option<String>,

    /// Keep the board session after publishing, useful when the next command needs ownership.
    #[arg(long)]
    pub keep_session: bool,
}

pub(super) async fn run(axvisor: &mut Axvisor, args: Args) -> anyhow::Result<()> {
    match args.command {
        Some(Command::Check(args)) => check(axvisor, args).await,
        Some(Command::Init(args)) => init(axvisor, args),
        Some(Command::Publish(args)) => publish(axvisor, args).await,
        None => publish(axvisor, args.publish).await,
    }
}

fn init(axvisor: &Axvisor, args: ArgsInit) -> anyhow::Result<()> {
    let output = if args.output.is_absolute() {
        args.output
    } else {
        axvisor.app.workspace_root().join(args.output)
    };
    if output.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        );
    }

    let config = render_init_config(&args.board, !args.no_open_console);
    fs::write(&output, config).with_context(|| format!("failed to write {}", output.display()))?;
    println!("generated {}", output.display());
    Ok(())
}

async fn check(axvisor: &mut Axvisor, args: ArgsCheck) -> anyhow::Result<()> {
    let request = axvisor.prepare_request(
        (&args.build).into(),
        None,
        None,
        SnapshotPersistence::Discard,
    )?;

    if request.arch != "x86_64" {
        bail!(
            "Axvisor HTTP Boot phase-0 check currently targets x86_64, got {}",
            request.arch
        );
    }

    axvisor.app.set_debug_mode(request.debug)?;

    let elf_path = match args.elf {
        Some(path) => path,
        None => {
            let cargo = super::build::load_cargo_config(&request)?;
            axvisor
                .app
                .build(cargo, request.build_info_path.clone())
                .await?;
            backtrace::arceos_rust_elf_path(
                axvisor.app.workspace_root(),
                &request.target,
                &request.package,
                request.debug,
            )
        }
    };

    let report = inspect_elf(&elf_path)?;
    report.print(&elf_path);
    Ok(())
}

async fn publish(axvisor: &mut Axvisor, args: ArgsPublish) -> anyhow::Result<()> {
    let publish_config = PublishConfig::load(
        args.httpboot_config.as_deref(),
        axvisor.app.workspace_root(),
    )?
    .merge_args(&args);
    let request = axvisor.prepare_request(
        (&args.build).into(),
        None,
        None,
        SnapshotPersistence::Discard,
    )?;

    if request.arch != "x86_64" {
        bail!(
            "Axvisor HTTP Boot publish currently targets x86_64, got {}",
            request.arch
        );
    }

    axvisor.app.set_debug_mode(request.debug)?;
    let elf_path = match args.elf {
        Some(path) => path,
        None => {
            let cargo = super::build::load_cargo_config(&request)?;
            axvisor
                .app
                .build(cargo, request.build_info_path.clone())
                .await?;
            backtrace::arceos_rust_elf_path(
                axvisor.app.workspace_root(),
                &request.target,
                &request.package,
                request.debug,
            )
        }
    };

    let report = inspect_elf(&elf_path)?;
    let kernel_bin = args.kernel_bin.unwrap_or_else(|| {
        axbuild_tmp_dir(axvisor.app.workspace_root())
            .join("httpboot")
            .join("axvisor-x86_64-kernel.bin")
    });
    write_flat_binary_from_elf(&elf_path, &report, &kernel_bin)?;

    let client = HttpBootApiClient::new(&publish_config.server, publish_config.port)?;
    let board_type = publish_config.board_type.as_deref().ok_or_else(|| {
        anyhow!(
            "missing HTTP Boot board; set `board = \"...\"` in .httpboot.toml, pass \
             `--httpboot-config PATH`, or override with `--board BOARD`"
        )
    })?;
    let session = client.create_session(board_type).await?;
    let session_guard = SessionGuard {
        client: client.clone(),
        session_id: session.session_id.clone(),
        keep: args.keep_session,
    };
    let heartbeat_guard = SessionHeartbeat::start(client.clone(), session.session_id.clone());

    println!("=== Axvisor x86_64 HTTP Boot publish ===");
    println!("board_type: {board_type}");
    println!("board_id: {}", session.board_id);
    println!("session_id: {}", session.session_id);

    let publish_result = publish_artifacts_for_session(
        &client,
        &session,
        &publish_config,
        &elf_path,
        &kernel_bin,
        &report,
        args.keep_session,
    )
    .await;
    let run_result = match publish_result {
        Ok(()) => run_httpboot_post_publish(&client, &session, &publish_config).await,
        Err(err) => Err(err),
    };
    heartbeat_guard.stop().await;
    let release_result = session_guard.release().await;
    run_result?;
    release_result?;
    Ok(())
}

async fn publish_artifacts_for_session(
    client: &HttpBootApiClient,
    session: &SessionCreatedResponse,
    publish_config: &PublishConfig,
    elf_path: &Path,
    kernel_bin: &Path,
    report: &HttpbootElfReport,
    keep_session: bool,
) -> anyhow::Result<()> {
    if session.boot_mode != "uefi_http" {
        bail!(
            "unsupported remote boot mode `{}`; only `uefi_http` is supported",
            session.boot_mode
        );
    }

    let boot_profile = client.get_boot_profile(&session.session_id).await?;
    let profile = boot_profile.uefi_http_profile()?;
    let remote_name = publish_config
        .remote_name
        .clone()
        .or(profile.kernel_file.clone())
        .unwrap_or_else(|| "kernel.bin".to_string());
    let kernel_load_addr = publish_config
        .kernel_load_addr
        .clone()
        .or(profile.kernel_load_addr.clone())
        .unwrap_or_else(|| hex(report.load_addr));
    let entry_point = publish_config.entry_point.clone().unwrap_or_else(|| {
        if report.httpboot_entry_symbol.is_some() {
            hex(report.entry_paddr)
        } else {
            profile
                .entry_point
                .clone()
                .unwrap_or_else(|| hex(report.entry_paddr))
        }
    });
    let arch = profile.boot_arch.as_deref().unwrap_or("x86_64").to_string();
    if arch != "x86_64" {
        bail!("HTTP Boot board arch is `{arch}`, expected `x86_64`");
    }

    validate_manifest_address("kernel_load_addr", &kernel_load_addr, report.load_addr)?;
    validate_manifest_address("entry_point", &entry_point, report.entry_paddr)?;

    let kernel_bytes = fs::read(&kernel_bin)
        .with_context(|| format!("failed to read {}", kernel_bin.display()))?;
    let kernel_size = kernel_bytes.len() as u64;
    let kernel_file = client
        .upload_http_boot_file(&session.session_id, &remote_name, kernel_bytes)
        .await
        .with_context(|| format!("failed to upload HTTP Boot file `{remote_name}`"))?;

    let manifest = HttpBootManifest {
        kernel_url: kernel_file.http_url.clone(),
        kernel_size,
        kernel_load_addr,
        entry_point,
        arch,
    };
    let manifest_file = client
        .upload_http_boot_manifest(&session.session_id, &manifest)
        .await
        .context("failed to upload HTTP Boot manifest")?;

    println!("elf: {}", elf_path.display());
    println!("kernel_bin: {}", kernel_bin.display());
    println!("kernel_size: {}", hex(kernel_size));
    println!("kernel_url: {}", kernel_file.http_url);
    println!("manifest_url: {}", manifest_file.http_url);
    println!(
        "session_release: {}",
        if keep_session { "kept" } else { "requested" }
    );
    Ok(())
}

async fn run_httpboot_post_publish(
    client: &HttpBootApiClient,
    session: &SessionCreatedResponse,
    publish_config: &PublishConfig,
) -> anyhow::Result<()> {
    println!("Waiting for board on power or reset...");

    if publish_config.power_cycle {
        client
            .power_off_board(&session.session_id)
            .await
            .context("failed to power off board")?;
        client
            .power_on_board(&session.session_id)
            .await
            .context("failed to power on board")?;
    }

    if !publish_config.open_console {
        return Ok(());
    }

    let serial_status = client
        .get_serial_status(&session.session_id)
        .await
        .with_context(|| {
            format!(
                "failed to get serial status for session `{}`",
                session.session_id
            )
        })?;

    if serial_status.available {
        let ws_path = serial_status
            .ws_url
            .as_deref()
            .or(session.ws_url.as_deref())
            .ok_or_else(|| anyhow!("server did not return a serial websocket URL"))?;
        if serial_status.connected {
            println!("serial_console: server reports an existing connection; trying anyway");
        }
        if let Some(port) = serial_status.port.as_deref() {
            if let Some(baud_rate) = serial_status.baud_rate {
                println!("serial_console: {port} @ {baud_rate}");
            } else {
                println!("serial_console: {port}");
            }
        }
        let ws_url = client.resolve_ws_url(ws_path)?;
        terminal::run_serial_terminal(ws_url).await
    } else {
        println!("Board has no serial configuration; keeping session alive until Ctrl+C.");
        println!("HTTP Boot artifacts are ready. Reset or power on the board now.");
        tokio::signal::ctrl_c()
            .await
            .context("failed to wait for Ctrl+C")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
struct PublishConfig {
    board_type: Option<String>,
    server: String,
    port: u16,
    remote_name: Option<String>,
    kernel_load_addr: Option<String>,
    entry_point: Option<String>,
    power_cycle: bool,
    open_console: bool,
}

impl PublishConfig {
    fn load(path: Option<&Path>, workspace_root: &Path) -> anyhow::Result<Self> {
        let mut config = Self {
            board_type: None,
            server: "127.0.0.1".to_string(),
            port: 2999,
            remote_name: Some("kernel.bin".to_string()),
            kernel_load_addr: None,
            entry_point: None,
            power_cycle: false,
            open_console: false,
        };

        let default_path = workspace_root.join(".httpboot.toml");
        let path = path
            .map(Path::to_path_buf)
            .or_else(|| default_path.exists().then_some(default_path));

        if let Some(path) = path {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let file_config: PublishConfigFile = toml::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            config.merge_file(file_config);
        }

        Ok(config)
    }

    fn merge_file(&mut self, file: PublishConfigFile) {
        if let Some(board) = file.board.or(file.board_type) {
            self.board_type = Some(board);
        }
        if let Some(server) = file.server {
            self.server = server;
        }
        if let Some(port) = file.port {
            self.port = port;
        }
        self.remote_name = file.remote_name.or(self.remote_name.take());
        self.kernel_load_addr = file.kernel_load_addr.or(self.kernel_load_addr.take());
        self.entry_point = file.entry_point.or(self.entry_point.take());
        if let Some(power_cycle) = file.power_cycle {
            self.power_cycle = power_cycle;
        }
        if let Some(open_console) = file.open_console {
            self.open_console = open_console;
        }
    }

    fn merge_args(mut self, args: &ArgsPublish) -> Self {
        if let Some(board_type) = args.board_type.clone() {
            self.board_type = Some(board_type);
        }
        if let Some(server) = args.server.clone() {
            self.server = server;
        }
        if let Some(port) = args.port {
            self.port = port;
        }
        if let Some(remote_name) = args.remote_name.clone() {
            self.remote_name = Some(remote_name);
        }
        if let Some(kernel_load_addr) = args.kernel_load_addr.clone() {
            self.kernel_load_addr = Some(kernel_load_addr);
        }
        if let Some(entry_point) = args.entry_point.clone() {
            self.entry_point = Some(entry_point);
        }
        self
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PublishConfigFile {
    board: Option<String>,
    board_type: Option<String>,
    server: Option<String>,
    port: Option<u16>,
    remote_name: Option<String>,
    kernel_load_addr: Option<String>,
    entry_point: Option<String>,
    power_cycle: Option<bool>,
    open_console: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentInfo {
    index: usize,
    vaddr: u64,
    paddr: u64,
    file_size: u64,
    mem_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpbootElfReport {
    entry: u64,
    load_addr: u64,
    image_file_size: u64,
    image_mem_size: u64,
    entry_offset: u64,
    entry_paddr: u64,
    start_symbol: Option<u64>,
    httpboot_entry_symbol: Option<u64>,
    main_symbol: Option<u64>,
    load_segments: Vec<SegmentInfo>,
}

impl HttpbootElfReport {
    fn print(&self, elf_path: &PathBuf) {
        println!("=== Axvisor x86_64 HTTP Boot phase-0 check ===");
        println!("elf: {}", elf_path.display());
        println!("kernel_load_addr: {}", hex(self.load_addr));
        println!("elf_entry: {}", hex(self.entry));
        println!("entry_offset_from_load: {}", hex(self.entry_offset));
        println!("entry_paddr_for_manifest: {}", hex(self.entry_paddr));
        println!("flat_binary_file_size: {}", hex(self.image_file_size));
        println!("flat_binary_memory_span: {}", hex(self.image_mem_size));

        for segment in &self.load_segments {
            println!(
                "load_segment[{}]: paddr={} vaddr={} filesz={} memsz={}",
                segment.index,
                hex(segment.paddr),
                hex(segment.vaddr),
                hex(segment.file_size),
                hex(segment.mem_size)
            );
        }

        println!("_start: {}", option_hex(self.start_symbol));
        println!("httpboot_entry: {}", option_hex(self.httpboot_entry_symbol));
        println!("main: {}", option_hex(self.main_symbol));
        if self.httpboot_entry_symbol.is_some() {
            println!("phase0_status: ready");
            println!(
                "phase0_note: image has a compact physical load span and exposes a direct HTTP \
                 Boot entry wrapper."
            );
        } else {
            println!("phase0_status: partial");
            println!(
                "phase0_note: image has a compact physical load span, but the current x86_64 \
                 entry is the Multiboot _start path. A direct HTTP Boot entry wrapper or \
                 Multiboot context synthesis is still required before jump."
            );
        }
        println!(
            "manifest_v1_candidate: kernel_load_addr={} entry_point={}",
            hex(self.load_addr),
            hex(self.entry_paddr)
        );
    }
}

fn inspect_elf(path: &PathBuf) -> anyhow::Result<HttpbootElfReport> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let elf: object::read::elf::ElfFile64<'_, object::Endianness> =
        object::read::elf::ElfFile64::parse(bytes.as_slice())
            .map_err(|e| anyhow!("failed to parse x86_64 ELF {}: {e}", path.display()))?;
    let endian = elf.endian();
    let entry: u64 = elf.elf_header().e_entry(endian).into();

    let mut load_segments = Vec::new();
    for (index, header) in elf.elf_program_headers().iter().enumerate() {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let file_size = header.p_filesz(endian).into();
        let mem_size = header.p_memsz(endian).into();
        if mem_size == 0 {
            continue;
        }
        load_segments.push(SegmentInfo {
            index,
            vaddr: header.p_vaddr(endian).into(),
            paddr: header.p_paddr(endian).into(),
            file_size,
            mem_size,
        });
    }

    if load_segments.is_empty() {
        bail!("{} has no loadable ELF segments", path.display());
    }

    load_segments.sort_by_key(|segment| segment.paddr);

    let load_addr = load_segments[0].paddr;
    let image_file_end = max_segment_end(&load_segments, |segment| segment.file_size)?;
    let image_mem_end = max_segment_end(&load_segments, |segment| segment.mem_size)?;
    let first = &load_segments[0];
    let entry_offset = entry
        .checked_sub(first.vaddr)
        .ok_or_else(|| anyhow!("ELF entry is below first load segment virtual address"))?;
    let entry_paddr = first
        .paddr
        .checked_add(entry_offset)
        .ok_or_else(|| anyhow!("entry physical address overflows u64"))?;
    let httpboot_entry_symbol = find_symbol(&elf, "httpboot_entry");
    let manifest_entry_paddr = match httpboot_entry_symbol {
        Some(symbol) => first
            .paddr
            .checked_add(symbol.checked_sub(first.vaddr).ok_or_else(|| {
                anyhow!("httpboot_entry is below first load segment virtual address")
            })?)
            .ok_or_else(|| anyhow!("httpboot_entry physical address overflows u64"))?,
        None => entry_paddr,
    };

    Ok(HttpbootElfReport {
        entry,
        load_addr,
        image_file_size: image_file_end - load_addr,
        image_mem_size: image_mem_end - load_addr,
        entry_offset,
        entry_paddr: manifest_entry_paddr,
        start_symbol: find_symbol(&elf, "_start"),
        httpboot_entry_symbol,
        main_symbol: find_symbol(&elf, "main"),
        load_segments,
    })
}

fn write_flat_binary_from_elf(
    elf_path: &Path,
    report: &HttpbootElfReport,
    output_path: &Path,
) -> anyhow::Result<()> {
    let bytes =
        fs::read(elf_path).with_context(|| format!("failed to read {}", elf_path.display()))?;
    let elf: object::read::elf::ElfFile64<'_, object::Endianness> =
        object::read::elf::ElfFile64::parse(bytes.as_slice())
            .map_err(|e| anyhow!("failed to parse x86_64 ELF {}: {e}", elf_path.display()))?;
    let endian = elf.endian();
    let image_len = usize::try_from(report.image_mem_size)
        .context("flat binary memory span does not fit usize")?;
    let mut image = vec![0; image_len];

    for header in elf.elf_program_headers() {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let file_size: u64 = header.p_filesz(endian).into();
        if file_size == 0 {
            continue;
        }
        let paddr: u64 = header.p_paddr(endian).into();
        let offset = paddr.checked_sub(report.load_addr).ok_or_else(|| {
            anyhow!(
                "PT_LOAD segment paddr {} is below load addr {}",
                hex(paddr),
                hex(report.load_addr)
            )
        })?;
        let output_start = usize::try_from(offset).context("segment output offset overflow")?;
        let output_len = usize::try_from(file_size).context("segment file size overflow")?;
        let output_end = output_start
            .checked_add(output_len)
            .context("segment output range overflow")?;
        if output_end > image.len() {
            bail!(
                "PT_LOAD segment ending at {} exceeds flat image span {}",
                hex(paddr + file_size),
                hex(report.load_addr + report.image_mem_size)
            );
        }

        let file_offset: u64 = header.p_offset(endian).into();
        let input_start = usize::try_from(file_offset).context("segment file offset overflow")?;
        let input_end = input_start
            .checked_add(output_len)
            .context("segment input range overflow")?;
        let data = bytes
            .get(input_start..input_end)
            .ok_or_else(|| anyhow!("PT_LOAD segment data is outside {}", elf_path.display()))?;
        image[output_start..output_end].copy_from_slice(data);
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(output_path, image)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    Ok(())
}

fn max_segment_end(
    segments: &[SegmentInfo],
    size: impl Fn(&SegmentInfo) -> u64,
) -> anyhow::Result<u64> {
    segments
        .iter()
        .map(|segment| {
            segment
                .paddr
                .checked_add(size(segment))
                .ok_or_else(|| anyhow!("load segment end address overflows u64"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("load segment list should not be empty")
}

fn find_symbol(elf: &object::read::elf::ElfFile64<'_>, name: &str) -> Option<u64> {
    elf.symbols()
        .find(|symbol| symbol.name().ok() == Some(name))
        .map(|symbol| symbol.address())
}

fn hex(value: u64) -> String {
    format!("{value:#x}")
}

fn option_hex(value: Option<u64>) -> String {
    value.map(hex).unwrap_or_else(|| "missing".to_string())
}

fn render_init_config(board: &str, open_console: bool) -> String {
    let mut config = format!(
        "# Local defaults for `cargo axvisor httpboot`.\n\
         # ostool-server defaults to http://127.0.0.1:2999.\n\n\
         board = \"{board}\"\n"
    );
    if open_console {
        config.push_str("open_console = true\n");
    }
    config
}

fn parse_hex_u64(value: &str) -> anyhow::Result<u64> {
    let value = value.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    u64::from_str_radix(value, 16).with_context(|| format!("invalid hex value `{value}`"))
}

fn validate_manifest_address(name: &str, actual: &str, expected: u64) -> anyhow::Result<()> {
    let actual_value = parse_hex_u64(actual)?;
    if actual_value != expected {
        bail!(
            "{name} `{actual}` does not match inspected ELF value {}",
            hex(expected)
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct HttpBootApiClient {
    client: Client,
    base_url: Url,
    ws_base_url: Url,
}

impl HttpBootApiClient {
    fn new(server: &str, port: u16) -> anyhow::Result<Self> {
        let base_url = build_base_url("http", server, port)?;
        let ws_base_url = build_base_url("ws", server, port)?;
        Ok(Self {
            client: Client::new(),
            base_url,
            ws_base_url,
        })
    }

    async fn create_session(&self, board_type: &str) -> anyhow::Result<SessionCreatedResponse> {
        self.decode_json(
            self.client
                .post(self.endpoint("/api/v1/sessions"))
                .json(&CreateSessionRequest {
                    board_type: board_type.to_string(),
                    required_tags: Vec::new(),
                    client_name: Some("axbuild-httpboot".to_string()),
                })
                .send()
                .await
                .context("failed to create ostool-server session")?,
        )
        .await
    }

    async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .delete(self.endpoint(&format!("/api/v1/sessions/{session_id}")))
            .send()
            .await
            .with_context(|| format!("failed to release ostool-server session `{session_id}`"))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        self.decode_empty(response).await
    }

    async fn heartbeat(&self, session_id: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .post(self.endpoint(&format!("/api/v1/sessions/{session_id}/heartbeat")))
            .send()
            .await
            .with_context(|| format!("failed to heartbeat ostool-server session `{session_id}`"))?;
        let _ignored: serde_json::Value = self.decode_json(response).await?;
        Ok(())
    }

    async fn get_boot_profile(&self, session_id: &str) -> anyhow::Result<BootProfileResponse> {
        self.decode_json(
            self.client
                .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/boot-profile")))
                .send()
                .await
                .with_context(|| {
                    format!("failed to get ostool-server boot profile for session `{session_id}`")
                })?,
        )
        .await
    }

    async fn get_serial_status(&self, session_id: &str) -> anyhow::Result<SerialStatusResponse> {
        self.decode_json(
            self.client
                .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/serial")))
                .send()
                .await
                .with_context(|| {
                    format!("failed to get ostool-server serial status for session `{session_id}`")
                })?,
        )
        .await
    }

    async fn power_on_board(&self, session_id: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .post(self.endpoint(&format!("/api/v1/sessions/{session_id}/board/power-on")))
            .send()
            .await
            .with_context(|| format!("failed to power on board for session `{session_id}`"))?;
        self.decode_empty(response).await
    }

    async fn power_off_board(&self, session_id: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .post(self.endpoint(&format!("/api/v1/sessions/{session_id}/board/power-off")))
            .send()
            .await
            .with_context(|| format!("failed to power off board for session `{session_id}`"))?;
        self.decode_empty(response).await
    }

    async fn upload_http_boot_file(
        &self,
        session_id: &str,
        relative_path: &str,
        bytes: Vec<u8>,
    ) -> anyhow::Result<HttpBootFileResponse> {
        self.decode_json(
            self.client
                .put(self.endpoint(&format!("/api/v1/sessions/{session_id}/http-boot/files")))
                .header("X-File-Path", relative_path)
                .body(bytes)
                .send()
                .await
                .with_context(|| format!("failed to upload HTTP Boot file `{relative_path}`"))?,
        )
        .await
    }

    async fn upload_http_boot_manifest(
        &self,
        session_id: &str,
        manifest: &HttpBootManifest,
    ) -> anyhow::Result<HttpBootFileResponse> {
        self.decode_json(
            self.client
                .put(self.endpoint(&format!("/api/v1/sessions/{session_id}/http-boot/manifest")))
                .json(manifest)
                .send()
                .await
                .context("failed to upload HTTP Boot manifest")?,
        )
        .await
    }

    fn endpoint(&self, path: &str) -> Url {
        self.base_url
            .join(path.trim_start_matches('/'))
            .expect("static API path should be valid")
    }

    fn resolve_ws_url(&self, ws_url: &str) -> anyhow::Result<Url> {
        if ws_url.starts_with("ws://") || ws_url.starts_with("wss://") {
            return Url::parse(ws_url).with_context(|| format!("invalid websocket URL `{ws_url}`"));
        }

        self.ws_base_url
            .join(ws_url)
            .with_context(|| format!("failed to resolve websocket URL `{ws_url}`"))
    }

    async fn decode_json<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> anyhow::Result<T> {
        let status = response.status();
        if status.is_success() {
            return response
                .json::<T>()
                .await
                .context("failed to decode ostool-server JSON response");
        }
        Err(api_error(status, response.text().await.unwrap_or_default()))
    }

    async fn decode_empty(&self, response: reqwest::Response) -> anyhow::Result<()> {
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(api_error(status, response.text().await.unwrap_or_default()))
    }
}

fn build_base_url(scheme: &str, server: &str, port: u16) -> anyhow::Result<Url> {
    let mut url = if server.starts_with("http://") || server.starts_with("https://") {
        let parsed =
            Url::parse(server).with_context(|| format!("invalid ostool-server URL `{server}`"))?;
        let target_scheme = match (scheme, parsed.scheme()) {
            ("ws", "http") => "ws",
            ("ws", "https") => "wss",
            ("http", "http") => "http",
            ("http", "https") => "https",
            _ => scheme,
        };
        let mut url = parsed;
        url.set_scheme(target_scheme)
            .map_err(|_| anyhow!("invalid ostool-server URL scheme `{server}`"))?;
        if url.port().is_none() {
            url.set_port(Some(port))
                .map_err(|_| anyhow!("invalid ostool-server port `{port}`"))?;
        }
        url
    } else {
        Url::parse(&format!("{scheme}://localhost"))
            .with_context(|| format!("failed to create {scheme} URL"))?
    };

    if !server.starts_with("http://") && !server.starts_with("https://") {
        url.set_host(Some(server))
            .map_err(|_| anyhow!("invalid ostool-server host `{server}`"))?;
        url.set_port(Some(port))
            .map_err(|_| anyhow!("invalid ostool-server port `{port}`"))?;
    }
    Ok(url)
}

#[derive(Debug, Serialize)]
struct CreateSessionRequest {
    board_type: String,
    required_tags: Vec<String>,
    client_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionCreatedResponse {
    session_id: String,
    board_id: String,
    boot_mode: String,
    ws_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BootProfileResponse {
    boot: RemoteBootConfig,
}

impl BootProfileResponse {
    fn uefi_http_profile(&self) -> anyhow::Result<&UefiHttpProfile> {
        match &self.boot {
            RemoteBootConfig::UefiHttp(profile) => Ok(profile),
            _ => bail!("server returned a non-uefi_http boot profile"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RemoteBootConfig {
    Uboot,
    Pxe,
    UefiHttp(UefiHttpProfile),
}

#[derive(Debug, Deserialize)]
struct UefiHttpProfile {
    boot_arch: Option<String>,
    kernel_file: Option<String>,
    kernel_load_addr: Option<String>,
    entry_point: Option<String>,
}

#[derive(Debug, Serialize)]
struct HttpBootManifest {
    kernel_url: String,
    kernel_size: u64,
    kernel_load_addr: String,
    entry_point: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
struct HttpBootFileResponse {
    http_url: String,
}

#[derive(Debug, Deserialize)]
struct SerialStatusResponse {
    available: bool,
    connected: bool,
    port: Option<String>,
    baud_rate: Option<u32>,
    ws_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiErrorResponse {
    message: String,
}

fn api_error(status: StatusCode, body: String) -> anyhow::Error {
    match serde_json::from_str::<ApiErrorResponse>(&body) {
        Ok(error) => anyhow!(
            "ostool-server request failed with {status}: {}",
            error.message
        ),
        Err(_) if !body.trim().is_empty() => {
            anyhow!(
                "ostool-server request failed with {status}: {}",
                body.trim()
            )
        }
        Err(_) => anyhow!("ostool-server request failed with {status}"),
    }
}

struct SessionGuard {
    client: HttpBootApiClient,
    session_id: String,
    keep: bool,
}

impl SessionGuard {
    async fn release(self) -> anyhow::Result<()> {
        if self.keep {
            return Ok(());
        }
        self.client.delete_session(&self.session_id).await
    }
}

struct SessionHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl SessionHeartbeat {
    fn start(client: HttpBootApiClient, session_id: String) -> Self {
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                interval.tick().await;
                if let Err(err) = client.heartbeat(&session_id).await {
                    eprintln!("session_heartbeat: {err:#}");
                }
            }
        });
        Self { handle }
    }

    async fn stop(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::{PublishConfig, PublishConfigFile, build_base_url, render_init_config};

    #[test]
    fn init_config_is_minimal() {
        let config = render_init_config("Asus-nuc15-x86_64-vmx", true);

        assert!(config.contains("board = \"Asus-nuc15-x86_64-vmx\""));
        assert!(config.contains("open_console = true"));
        assert!(!config.contains("server ="));
        assert!(!config.contains("port ="));
        assert!(!config.contains("kernel_load_addr ="));
        assert!(!config.contains("entry_point ="));
        assert!(!config.contains("efi_loader_path ="));
    }

    #[test]
    fn publish_config_file_enables_post_publish_console() {
        let file: PublishConfigFile = toml::from_str(
            r#"
            board = "x86-httpboot"
            power_cycle = true
            open_console = true
            "#,
        )
        .unwrap();
        let mut config = PublishConfig {
            board_type: None,
            server: "127.0.0.1".to_string(),
            port: 2999,
            remote_name: Some("kernel.bin".to_string()),
            kernel_load_addr: None,
            entry_point: None,
            power_cycle: false,
            open_console: false,
        };

        config.merge_file(file);

        assert_eq!(config.board_type.as_deref(), Some("x86-httpboot"));
        assert!(config.power_cycle);
        assert!(config.open_console);
    }

    #[test]
    fn build_base_url_accepts_full_server_url() {
        let http = build_base_url("http", "https://10.3.10.229:9443", 2999).unwrap();
        let ws = build_base_url("ws", "https://10.3.10.229:9443", 2999).unwrap();

        assert_eq!(http.as_str(), "https://10.3.10.229:9443/");
        assert_eq!(ws.as_str(), "wss://10.3.10.229:9443/");
    }

    #[test]
    fn build_base_url_adds_default_port_for_host_only_server() {
        let http = build_base_url("http", "10.3.10.229", 2999).unwrap();
        let ws = build_base_url("ws", "10.3.10.229", 2999).unwrap();

        assert_eq!(http.as_str(), "http://10.3.10.229:2999/");
        assert_eq!(ws.as_str(), "ws://10.3.10.229:2999/");
    }
}
