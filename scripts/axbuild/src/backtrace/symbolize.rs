use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, bail};
use object::{Object, ObjectSymbol, SymbolKind};

use super::{
    HOST_SYMBOLIZE_HEADER, SymbolizeArgs,
    parser::{Block, infer_kind_filter, parse_blocks},
};

/// Result of post-QEMU host symbolize; drives whether the capture log may be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolizeAfterQemuOutcome {
    /// No log, no backtrace markers, or no parseable blocks: nothing symbolized.
    Skipped,
    /// Parsed blocks and emitted symbolized output.
    Symbolized,
    /// Backtrace data present but read/parse/ELF/load/output failed: retain log for debug.
    Failed,
}

/// True when `TGOSKITS_KEEP_QEMU_LOG` is set to a truthy value (`1`, `true`, `yes`, case-insensitive).
pub(crate) fn keep_qemu_log_from_env() -> bool {
    std::env::var("TGOSKITS_KEEP_QEMU_LOG")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
}

/// Whether captured raw blocks should be written to `log_path` after QEMU.
pub(crate) fn should_persist_qemu_capture_log(
    keep_log: bool,
    outcome: SymbolizeAfterQemuOutcome,
    has_captured_blocks: bool,
) -> bool {
    has_captured_blocks && (keep_log || outcome == SymbolizeAfterQemuOutcome::Failed)
}

/// Whether a successful symbolize should remove the QEMU capture log.
pub(crate) fn should_delete_qemu_log_after_symbolize(
    outcome: SymbolizeAfterQemuOutcome,
    keep_log: bool,
) -> bool {
    !keep_log && outcome == SymbolizeAfterQemuOutcome::Symbolized
}

/// Remove the QEMU log when symbolize succeeded and retention was not requested.
pub(crate) fn apply_qemu_log_retention(
    log: &Path,
    outcome: SymbolizeAfterQemuOutcome,
    keep_log: bool,
) -> anyhow::Result<()> {
    if !should_delete_qemu_log_after_symbolize(outcome, keep_log) {
        return Ok(());
    }
    remove_qemu_log_file(log)
}

fn remove_qemu_log_file(log: &Path) -> anyhow::Result<()> {
    match fs::remove_file(log) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            eprintln!(
                "warning: failed to remove qemu log {} after symbolize: {err:#}",
                log.display()
            );
            Ok(())
        }
    }
}

/// Host symbolizer for pipe-captured blocks; ELF is validated before QEMU, loaded on symbolize.
pub(crate) struct BacktraceSymbolizeSession {
    elf: PathBuf,
    case_name: String,
    header_printed: AtomicBool,
    symbolized: AtomicBool,
    failed: AtomicBool,
    symbolizer: OnceLock<Option<HostSymbolizer>>,
}

impl BacktraceSymbolizeSession {
    /// Validate ELF and prepare stream symbolize.
    ///
    /// The Loader is eagerly initialized here so we don't pay the ELF parsing
    /// cost again on the first `on_block_complete` call.
    ///
    /// Clippy: `addr2line::Loader` may not be `Sync`, making
    /// `BacktraceSymbolizeSession` non-`Sync` and triggering
    /// `arc_with_non_send_sync`. This is safe because all `Loader` access
    /// goes through `OnceLock`'s synchronized API.
    #[allow(clippy::arc_with_non_send_sync)]
    pub(crate) fn try_new(elf: &Path, case_name: &str) -> Option<Arc<Self>> {
        if !elf.is_file() {
            eprintln!(
                "warning: skipping stream backtrace symbolize; ELF not found at {}",
                elf.display()
            );
            return None;
        }
        let symbolizer = match HostSymbolizer::new(elf) {
            Ok(symbolizer) => Some(symbolizer),
            Err(err) => {
                eprintln!(
                    "warning: failed to load symbols from {} for stream backtrace symbolize: {err}",
                    elf.display()
                );
                return None;
            }
        };
        let once = OnceLock::new();
        once.set(symbolizer).ok();
        Some(Arc::new(Self {
            elf: elf.to_path_buf(),
            case_name: case_name.to_string(),
            header_printed: AtomicBool::new(false),
            symbolized: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            symbolizer: once,
        }))
    }

    /// Get or lazily initialize the cached symbolizer.
    fn symbolizer(&self) -> Option<&HostSymbolizer> {
        let opt = self
            .symbolizer
            .get_or_init(|| match HostSymbolizer::new(&self.elf) {
                Ok(symbolizer) => Some(symbolizer),
                Err(err) => {
                    eprintln!(
                        "warning: failed to load symbols from {} for stream backtrace symbolize: \
                         {err}",
                        self.elf.display()
                    );
                    None
                }
            });
        opt.as_ref()
    }

    pub(crate) fn streamed_symbolized(&self) -> bool {
        self.symbolized.load(Ordering::SeqCst)
    }

    pub(crate) fn streamed_failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    pub(crate) fn on_block_complete(&self, block_lines: &[String]) {
        if block_lines.is_empty() {
            return;
        }
        let text = block_lines.join("\n");
        let text = format!("{text}\n");
        let blocks = match parse_blocks(&text) {
            Ok(blocks) if !blocks.is_empty() => blocks,
            Ok(_) => return,
            Err(err) => {
                eprintln!(
                    "warning: failed to parse backtrace block during stream symbolize: {err:#}"
                );
                self.failed.store(true, Ordering::SeqCst);
                return;
            }
        };

        let Some(symbolizer) = self.symbolizer() else {
            self.failed.store(true, Ordering::SeqCst);
            return;
        };

        if !self.header_printed.swap(true, Ordering::SeqCst) {
            println!("\n{HOST_SYMBOLIZE_HEADER}");
        }

        let kind_filter = infer_kind_filter(&self.case_name, &blocks);
        let mut stdout = std::io::stdout().lock();
        if let Err(err) = write_symbolized_blocks(
            &mut stdout,
            symbolizer,
            &blocks,
            kind_filter.as_deref(),
            true,
            0,
        ) {
            eprintln!("warning: stream backtrace symbolize output failed: {err:#}");
            self.failed.store(true, Ordering::SeqCst);
            return;
        }
        self.symbolized.store(true, Ordering::SeqCst);
    }
}

fn captured_blocks_to_text(blocks: &[Vec<String>]) -> String {
    let mut text = String::new();
    for block in blocks {
        for line in block {
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

pub(crate) fn symbolize_captured_blocks_to_string(
    elf_path: &Path,
    case_name: &str,
    blocks: &[Vec<String>],
) -> anyhow::Result<Option<String>> {
    symbolize_text_to_string(elf_path, case_name, &captured_blocks_to_text(blocks))
}

pub(super) fn symbolize_text_to_string(
    elf_path: &Path,
    case_name: &str,
    text: &str,
) -> anyhow::Result<Option<String>> {
    if !text.contains("BACKTRACE_BEGIN") {
        return Ok(None);
    }

    let blocks = parse_blocks(text)?;
    if blocks.is_empty() {
        return Ok(None);
    }

    let symbolizer = HostSymbolizer::new(elf_path).map_err(|err| {
        anyhow::anyhow!("failed to load symbols from {}: {err}", elf_path.display())
    })?;
    let mut out = Vec::new();
    writeln!(&mut out, "{HOST_SYMBOLIZE_HEADER}")?;
    let kind_filter = infer_kind_filter(case_name, &blocks);
    write_symbolized_blocks(
        &mut out,
        &symbolizer,
        &blocks,
        kind_filter.as_deref(),
        true,
        0,
    )?;
    Ok(Some(String::from_utf8(out)?))
}

/// Write in-memory raw backtrace blocks to `log_path` (creates parent dirs).
pub(super) fn write_captured_blocks_to_log(
    log_path: &Path,
    blocks: &[Vec<String>],
) -> io::Result<()> {
    if blocks.is_empty() {
        return Ok(());
    }
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)?;
    for block in blocks {
        for line in block {
            writeln!(file, "{line}")?;
        }
    }
    file.flush()
}

fn finalize_qemu_capture_log(
    log: &Path,
    keep_log: bool,
    outcome: SymbolizeAfterQemuOutcome,
    memory_blocks: Option<&[Vec<String>]>,
) -> anyhow::Result<()> {
    let has_blocks = memory_blocks.is_some_and(|b| !b.is_empty());
    if should_persist_qemu_capture_log(keep_log, outcome, has_blocks)
        && let Some(blocks) = memory_blocks
    {
        write_captured_blocks_to_log(log, blocks)?;
    }
    apply_qemu_log_retention(log, outcome, keep_log)
}

/// After a QEMU run, symbolize any raw backtrace blocks without failing the test.
///
/// When a [`BacktraceSymbolizeSession`] already printed symbolized output during capture,
/// skips re-reading the log. On the default success path the log file is never created;
/// use `memory_blocks` for `--keep-qemu-log` persistence and stream-failure fallback.
pub(crate) fn maybe_symbolize_after_qemu(
    elf: &Path,
    log: &Path,
    case_name: &str,
    keep_log: bool,
    stream_session: Option<&BacktraceSymbolizeSession>,
    memory_blocks: Option<&[Vec<String>]>,
) -> anyhow::Result<SymbolizeAfterQemuOutcome> {
    let memory_has_blocks = memory_blocks.is_some_and(|b| !b.is_empty());

    if let Some(session) = stream_session
        && session.streamed_symbolized()
    {
        let outcome = SymbolizeAfterQemuOutcome::Symbolized;
        finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
        return Ok(outcome);
    }
    if let Some(session) = stream_session
        && session.streamed_failed()
    {
        // Fall through to memory/file-based symbolize as a second chance.
    }

    let text = if memory_has_blocks {
        captured_blocks_to_text(memory_blocks.unwrap())
    } else if log.is_file() {
        match fs::read_to_string(log) {
            Ok(text) => text,
            Err(err) => {
                eprintln!(
                    "warning: failed to read qemu log {} for backtrace symbolize: {err:#}",
                    log.display()
                );
                let outcome = SymbolizeAfterQemuOutcome::Failed;
                finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
                return Ok(outcome);
            }
        }
    } else {
        return Ok(SymbolizeAfterQemuOutcome::Skipped);
    };
    if !text.contains("BACKTRACE_BEGIN") {
        return Ok(SymbolizeAfterQemuOutcome::Skipped);
    }
    if !elf.is_file() {
        eprintln!(
            "warning: skipping backtrace symbolize; ELF not found at {}",
            elf.display()
        );
        let outcome = SymbolizeAfterQemuOutcome::Failed;
        finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
        return Ok(outcome);
    }

    let blocks = match parse_blocks(&text) {
        Ok(blocks) if !blocks.is_empty() => blocks,
        Ok(_) => return Ok(SymbolizeAfterQemuOutcome::Skipped),
        Err(err) => {
            eprintln!("warning: failed to parse backtrace blocks in qemu log: {err:#}");
            let outcome = SymbolizeAfterQemuOutcome::Failed;
            finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
            return Ok(outcome);
        }
    };

    let kind_filter = infer_kind_filter(case_name, &blocks);
    let symbolizer = match HostSymbolizer::new(elf) {
        Ok(symbolizer) => symbolizer,
        Err(err) => {
            eprintln!(
                "warning: failed to load symbols from {} for backtrace symbolize: {err}",
                elf.display()
            );
            let outcome = SymbolizeAfterQemuOutcome::Failed;
            finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
            return Ok(outcome);
        }
    };

    println!("\n{HOST_SYMBOLIZE_HEADER}");
    let mut stdout = std::io::stdout().lock();
    if let Err(err) = write_symbolized_blocks(
        &mut stdout,
        &symbolizer,
        &blocks,
        kind_filter.as_deref(),
        true,
        0,
    ) {
        eprintln!("warning: backtrace symbolize output failed: {err:#}");
        let outcome = SymbolizeAfterQemuOutcome::Failed;
        finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
        return Ok(outcome);
    }

    let outcome = SymbolizeAfterQemuOutcome::Symbolized;
    finalize_qemu_capture_log(log, keep_log, outcome, memory_blocks)?;
    Ok(outcome)
}

fn read_text(log: Option<&Path>) -> anyhow::Result<String> {
    match log {
        Some(path) => Ok(fs::read_to_string(path)
            .with_context(|| format!("failed to read log {}", path.display()))?),
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("failed to read stdin")?;
            Ok(s)
        }
    }
}

pub(super) fn symbolize_cli(args: SymbolizeArgs) -> anyhow::Result<()> {
    let text = read_text(args.log.as_deref())?;
    let blocks = parse_blocks(&text)?;
    if blocks.is_empty() {
        bail!("no backtrace blocks found");
    }

    let symbolizer = HostSymbolizer::new(&args.elf).map_err(|err| {
        anyhow::anyhow!(
            "failed to load dwarf/symbols from {}: {}",
            args.elf.display(),
            err
        )
    })?;

    write_symbolized_blocks(
        &mut std::io::stdout().lock(),
        &symbolizer,
        &blocks,
        args.kind.as_deref(),
        args.adjust_ip,
        args.ip_bias,
    )
}

/// Per-arch IP adjustment: return address minus this value falls within the
/// calling function. Matches the kernel-side `Frame::adjust_ip()` constants.
fn ip_adjustment_for_arch(arch: Option<&str>) -> u64 {
    match arch {
        Some("x86_64") => 1,
        Some("aarch64") => 4,
        Some("riscv64") | Some("riscv32") => 2,
        Some("loongarch64") => 4,
        _ => 1,
    }
}

pub(super) fn write_symbolized_blocks(
    out: &mut impl Write,
    symbolizer: &HostSymbolizer,
    blocks: &[Block],
    kind_filter: Option<&str>,
    adjust_ip: bool,
    ip_bias: i64,
) -> anyhow::Result<()> {
    for (i, block) in blocks.iter().enumerate() {
        if let Some(kind) = kind_filter
            && block.kind != kind
        {
            continue;
        }

        writeln!(
            out,
            "BACKTRACE_BLOCK {} kind={} arch={}",
            i,
            block.kind,
            block.arch.as_deref().unwrap_or("?")
        )?;

        let adj = ip_adjustment_for_arch(block.arch.as_deref());
        for frame in &block.frames {
            let ip = if adjust_ip && frame.ip > 0 {
                frame.ip.checked_sub(adj).unwrap_or(frame.ip)
            } else {
                frame.ip
            };
            let ip = ip.wrapping_add_signed(ip_bias);
            let symbolized = symbolizer.maybe_symbolize(ip);

            match (&frame.fp, symbolized) {
                (Some(fp), Some(sym)) => {
                    writeln!(
                        out,
                        "BT {} ip=0x{:x} fp=0x{:x} {}",
                        frame.idx, frame.ip, fp, sym
                    )?;
                }
                (Some(fp), None) => {
                    writeln!(out, "BT {} ip=0x{:x} fp=0x{:x}", frame.idx, frame.ip, fp)?;
                }
                (None, Some(sym)) => {
                    writeln!(out, "BT {} ip=0x{:x} {}", frame.idx, frame.ip, sym)?;
                }
                (None, None) => {
                    writeln!(out, "BT {} ip=0x{:x}", frame.idx, frame.ip)?;
                }
            };
        }

        for err in &block.errors {
            writeln!(out, "BT_ERROR {err}")?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct TextSymbol {
    pub(super) address: u64,
    pub(super) size: u64,
    pub(super) name: String,
}

pub(super) struct HostSymbolizer {
    loader: addr2line::Loader,
    pub(super) text_symbols: Vec<TextSymbol>,
}

impl HostSymbolizer {
    pub(super) fn new(elf: &Path) -> anyhow::Result<Self> {
        let loader = addr2line::Loader::new(elf).map_err(|err| anyhow::anyhow!("{err}"))?;
        let text_symbols = load_text_symbols(elf)?;
        Ok(Self {
            loader,
            text_symbols,
        })
    }

    pub(super) fn maybe_symbolize(&self, ip: u64) -> Option<String> {
        if ip == 0 {
            return None;
        }
        self.symbolize(ip)
    }

    pub(super) fn symbolize(&self, ip: u64) -> Option<String> {
        let mut frames = self.loader.find_frames(ip).ok()?;
        let mut out = Vec::new();
        while let Some(frame) = frames.next().ok()? {
            let name = frame.function.as_ref().and_then(|f| {
                let raw = f.raw_name().ok()?;
                self.display_symbol_name(raw.as_ref(), ip)
            });
            let loc = frame.location.as_ref().and_then(|l| {
                let file = l.file?;
                let line = l.line?;
                Some(format!("{file}:{line}"))
            });
            match (name, loc) {
                (Some(name), Some(loc)) => out.push(format!("{name} ({loc})")),
                (Some(name), None) => out.push(name),
                (None, Some(loc)) => out.push(loc),
                (None, None) => {}
            }
        }
        if out.is_empty() {
            let sym = self
                .loader
                .find_symbol(ip)
                .and_then(|name| self.display_symbol_name(name, ip))
                .or_else(|| self.nearest_text_symbol(ip));
            return sym;
        }

        Some(out.join(" ; "))
    }

    pub(super) fn display_symbol_name(&self, raw: &str, ip: u64) -> Option<String> {
        if is_compiler_local_symbol(raw) {
            self.nearest_text_symbol(ip)
        } else {
            Some(rustc_demangle::demangle(raw).to_string())
        }
    }

    fn nearest_text_symbol(&self, ip: u64) -> Option<String> {
        let idx = self.text_symbols.partition_point(|sym| sym.address <= ip);
        for sym in self.text_symbols[..idx].iter().rev() {
            if sym.size == 0 || ip < sym.address.saturating_add(sym.size) {
                return Some(rustc_demangle::demangle(&sym.name).to_string());
            }
        }
        None
    }
}

fn load_text_symbols(elf: &Path) -> anyhow::Result<Vec<TextSymbol>> {
    let bytes = fs::read(elf)?;
    let file = object::File::parse(bytes.as_slice())?;
    let mut symbols = Vec::new();

    for sym in file.symbols() {
        if sym.kind() != SymbolKind::Text || sym.address() == 0 {
            continue;
        }
        let Ok(name) = sym.name() else {
            continue;
        };
        if is_compiler_local_symbol(name) {
            continue;
        }
        symbols.push(TextSymbol {
            address: sym.address(),
            size: sym.size(),
            name: name.to_string(),
        });
    }

    symbols.sort_by(|a, b| {
        a.address
            .cmp(&b.address)
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
    symbols.dedup_by(|a, b| a.address == b.address && a.name == b.name);
    Ok(symbols)
}

pub(super) fn is_compiler_local_symbol(name: &str) -> bool {
    let name = name.trim();
    // Covered target-local labels include LLVM/GNU `.L*` labels observed on
    // x86_64/riscv64/loongarch64 and ARM/AArch64 mapping symbols such as
    // `$x`/`$d`; all are rendering noise for host backtrace output.
    name.starts_with(".L") || name.starts_with('$')
}
