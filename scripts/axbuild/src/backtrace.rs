use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use object::{Object, ObjectSymbol, SymbolKind};
use regex::Regex;

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
        Command::Symbolize(args) => symbolize_cli(args),
    }
}

const HOST_SYMBOLIZE_HEADER: &str = "=== host backtrace symbolize ===";

/// Resolved ELF path for an ArceOS std test package built via the workspace `target/` dir.
pub(crate) fn std_test_elf_path(
    workspace_root: &Path,
    target: &str,
    package: &str,
    debug: bool,
) -> PathBuf {
    let profile = if debug { "debug" } else { "release" };
    workspace_root
        .join("target")
        .join(std_test_target_dir(target))
        .join(profile)
        .join(package)
}

fn std_test_target_dir(target: &str) -> &str {
    if target.starts_with("x86_64-") {
        "x86_64-unknown-linux-musl"
    } else if target.starts_with("aarch64-") {
        "aarch64-unknown-linux-musl"
    } else if target.starts_with("riscv64") {
        "riscv64gc-unknown-linux-musl"
    } else if target.starts_with("loongarch64-") {
        "loongarch64-unknown-linux-musl"
    } else {
        target
    }
}

fn case_name_kind_hint(case_name: &str) -> Option<&'static str> {
    const KINDS: &[&str] = &["raw", "panic", "trap"];
    for segment in case_name.split(['/', '-']) {
        for kind in KINDS {
            if segment == *kind {
                return Some(kind);
            }
        }
    }
    if case_name.ends_with("-raw") {
        return Some("raw");
    }
    if case_name.ends_with("-panic") {
        return Some("panic");
    }
    if case_name.ends_with("-trap") {
        return Some("trap");
    }
    None
}

/// Infer a `kind=` filter for symbolize: case-name hints, else single block kind, else all kinds.
fn infer_kind_filter(case_name: &str, blocks: &[Block]) -> Option<String> {
    if let Some(kind) = case_name_kind_hint(case_name) {
        return Some(kind.to_string());
    }

    let kinds: HashSet<&str> = blocks.iter().map(|block| block.kind.as_str()).collect();
    if kinds.len() == 1 {
        return kinds.into_iter().next().map(str::to_string);
    }
    None
}

/// Whether captured raw blocks should be written to `log_path` after QEMU.
pub(crate) fn should_persist_qemu_capture_log(
    keep_log: bool,
    outcome: SymbolizeAfterQemuOutcome,
    has_captured_blocks: bool,
) -> bool {
    has_captured_blocks && (keep_log || outcome == SymbolizeAfterQemuOutcome::Failed)
}

/// Result of post-QEMU host symbolize; drives whether the capture log may be deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolizeAfterQemuOutcome {
    /// No log, no backtrace markers, or no parseable blocks — nothing symbolized.
    Skipped,
    /// Parsed blocks and emitted symbolized output.
    Symbolized,
    /// Backtrace data present but read/parse/ELF/load/output failed — retain log for debug.
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
        once.set(symbolizer).ok(); // always succeeds (first set)
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

pub(crate) fn symbolize_text_to_string(
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
pub(crate) fn write_captured_blocks_to_log(
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

#[derive(Debug, Clone)]
struct Frame {
    idx: usize,
    ip: u64,
    fp: Option<u64>,
}

#[derive(Debug, Clone)]
struct Block {
    kind: String,
    arch: Option<String>,
    frames: Vec<Frame>,
    errors: Vec<String>,
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

/// QEMU backtrace capture: optional on-disk log plus optional stream symbolize session.
#[derive(Clone)]
pub(crate) struct BacktraceQemuCapture {
    /// Path used when persisting raw blocks (`--keep-qemu-log` or failure/debug).
    pub log_path: PathBuf,
    pub stream_symbolize: Option<Arc<BacktraceSymbolizeSession>>,
    /// When true, raw `BACKTRACE_*` / `BT` lines are omitted from the terminal tee.
    pub suppress_terminal_raw_blocks: bool,
    /// When true, append raw blocks to `log_path` during QEMU (debug retention).
    pub write_log_during_capture: bool,
    /// All complete raw blocks captured during QEMU (for stream symbolize and deferred log write).
    pub captured_blocks: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

/// Incremental state machine: captures `BACKTRACE_BEGIN` … `BACKTRACE_END` blocks to memory
/// and optionally to a log file while a block is open.
pub(crate) struct BacktraceBlockCapture {
    log: Option<fs::File>,
    pending_stream_blocks: Option<Arc<std::sync::Mutex<Vec<Vec<String>>>>>,
    state: BlockCaptureState,
    line_buf: String,
    block_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockCaptureState {
    Idle,
    InBlock,
}

impl BacktraceBlockCapture {
    pub(crate) fn create(
        log_path: Option<&Path>,
        pending_stream_blocks: Option<Arc<std::sync::Mutex<Vec<Vec<String>>>>>,
    ) -> io::Result<Self> {
        let log = if let Some(path) = log_path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            Some(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            )
        } else {
            None
        };
        Ok(Self {
            log,
            pending_stream_blocks,
            state: BlockCaptureState::Idle,
            line_buf: String::new(),
            block_lines: Vec::new(),
        })
    }

    pub(crate) fn push_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        self.line_buf.push_str(&String::from_utf8_lossy(data));
        while let Some(newline) = self.line_buf.find('\n') {
            let line = self.line_buf[..newline].to_string();
            self.line_buf.drain(..=newline);
            let _ = self.process_line(&line)?;
        }
        Ok(())
    }

    /// Process guest bytes for log/stream symbolize; return bytes safe to write to the terminal.
    pub(crate) fn push_bytes_for_tee(
        &mut self,
        data: &[u8],
        suppress_raw_blocks: bool,
    ) -> io::Result<Vec<u8>> {
        if !suppress_raw_blocks {
            self.push_bytes(data)?;
            return Ok(data.to_vec());
        }

        let mut terminal_out = Vec::new();
        self.line_buf.push_str(&String::from_utf8_lossy(data));
        while let Some(newline) = self.line_buf.find('\n') {
            let line = self.line_buf[..newline].to_string();
            self.line_buf.drain(..=newline);
            if self.process_line(&line)? {
                terminal_out.extend_from_slice(line.as_bytes());
                terminal_out.push(b'\n');
            }
        }
        Ok(terminal_out)
    }

    pub(crate) fn finish(&mut self) -> io::Result<()> {
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            let _ = self.process_line(&line)?;
        }
        if self.state == BlockCaptureState::InBlock {
            self.flush_block()?;
            self.state = BlockCaptureState::Idle;
        }
        if let Some(log) = &mut self.log {
            log.flush()?;
        }
        Ok(())
    }

    /// Returns whether the line should be forwarded to the terminal when raw blocks are suppressed.
    fn process_line(&mut self, line: &str) -> io::Result<bool> {
        let has_begin = line.contains("BACKTRACE_BEGIN");
        let has_end = line.contains("BACKTRACE_END");
        let mut emit_terminal = true;

        match self.state {
            BlockCaptureState::Idle => {
                if has_begin {
                    emit_terminal = false;
                    self.block_lines.clear();
                    self.block_lines.push(line.to_string());
                    self.state = BlockCaptureState::InBlock;
                    if has_end {
                        self.flush_block()?;
                        self.state = BlockCaptureState::Idle;
                    }
                }
            }
            BlockCaptureState::InBlock => {
                emit_terminal = false;
                if has_begin && !self.block_lines.is_empty() {
                    self.flush_block()?;
                    self.block_lines.clear();
                }
                self.block_lines.push(line.to_string());
                if has_end {
                    self.flush_block()?;
                    self.block_lines.clear();
                    self.state = BlockCaptureState::Idle;
                }
            }
        }
        Ok(emit_terminal)
    }

    fn flush_block(&mut self) -> io::Result<()> {
        if self.block_lines.is_empty() {
            return Ok(());
        }
        if let Some(log) = &mut self.log {
            for line in &self.block_lines {
                writeln!(log, "{line}")?;
            }
        }
        if let Some(pending) = &self.pending_stream_blocks
            && let Ok(mut queue) = pending.lock()
        {
            queue.push(self.block_lines.clone());
        }
        self.block_lines.clear();
        Ok(())
    }
}

/// Symbolize blocks queued during pipe capture (runs on the main thread after QEMU).
pub(crate) fn flush_pending_stream_symbolize(
    session: &BacktraceSymbolizeSession,
    pending: &std::sync::Mutex<Vec<Vec<String>>>,
) {
    let blocks: Vec<Vec<String>> = match pending.lock() {
        Ok(queue) => queue.clone(),
        Err(_) => return,
    };
    for lines in &blocks {
        session.on_block_complete(lines);
    }
}

/// Filter a full QEMU transcript down to raw backtrace blocks and write them to `log_path`.
#[cfg(test)]
pub(crate) fn write_raw_blocks_from_output(output: &str, log_path: &Path) -> io::Result<()> {
    let mut capture = BacktraceBlockCapture::create(Some(log_path), None)?;
    capture.push_bytes(output.as_bytes())?;
    capture.finish()
}

fn parse_blocks(text: &str) -> anyhow::Result<Vec<Block>> {
    let begin_re = Regex::new(r"BACKTRACE_BEGIN\b.*\bkind=([^\s]+)\b(?:.*\barch=([^\s]+)\b)?")
        .context("invalid begin regex")?;
    let frame_re = Regex::new(r"\bBT\s+(\d+)\s+ip=0x([0-9a-fA-F]+)(?:\s+fp=0x([0-9a-fA-F]+))?")
        .context("invalid frame regex")?;
    let error_re = Regex::new(r"\bBT_ERROR\s+([^\s]+)").context("invalid error regex")?;
    let end_re = Regex::new(r"BACKTRACE_END\b").context("invalid end regex")?;

    #[derive(Debug)]
    enum State {
        Idle,
        Capturing(Block),
    }

    let mut state = State::Idle;
    let mut out = Vec::new();

    for line in text.lines() {
        match &mut state {
            State::Idle => {
                if let Some(cap) = begin_re.captures(line) {
                    let kind = cap.get(1).unwrap().as_str().to_string();
                    let arch = cap.get(2).map(|m| m.as_str().to_string());
                    state = State::Capturing(Block {
                        kind,
                        arch,
                        frames: Vec::new(),
                        errors: Vec::new(),
                    });
                }
            }
            State::Capturing(block) => {
                if let Some(cap) = begin_re.captures(line) {
                    out.push(block.clone());
                    let kind = cap.get(1).unwrap().as_str().to_string();
                    let arch = cap.get(2).map(|m| m.as_str().to_string());
                    *block = Block {
                        kind,
                        arch,
                        frames: Vec::new(),
                        errors: Vec::new(),
                    };
                    continue;
                }
                if end_re.is_match(line) {
                    out.push(block.clone());
                    state = State::Idle;
                    continue;
                }

                if let Some(cap) = frame_re.captures(line) {
                    let idx: usize = cap.get(1).unwrap().as_str().parse()?;
                    let ip = u64::from_str_radix(cap.get(2).unwrap().as_str(), 16)?;
                    let fp = cap
                        .get(3)
                        .map(|m| u64::from_str_radix(m.as_str(), 16))
                        .transpose()?;
                    block.frames.push(Frame { idx, ip, fp });
                    continue;
                }

                if let Some(cap) = error_re.captures(line) {
                    let err = cap.get(1).unwrap().as_str().to_string();
                    block.errors.push(err);
                }
            }
        }
    }

    if let State::Capturing(block) = state {
        out.push(block);
    }

    Ok(out)
}

fn symbolize_cli(args: SymbolizeArgs) -> anyhow::Result<()> {
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
        _ => 1, // unknown / missing → default sub(1), backward compatible
    }
}

fn write_symbolized_blocks(
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
struct TextSymbol {
    address: u64,
    size: u64,
    name: String,
}

struct HostSymbolizer {
    loader: addr2line::Loader,
    text_symbols: Vec<TextSymbol>,
}

impl HostSymbolizer {
    fn new(elf: &Path) -> anyhow::Result<Self> {
        let loader = addr2line::Loader::new(elf).map_err(|err| anyhow::anyhow!("{err}"))?;
        let text_symbols = load_text_symbols(elf)?;
        Ok(Self {
            loader,
            text_symbols,
        })
    }

    fn maybe_symbolize(&self, ip: u64) -> Option<String> {
        if ip == 0 {
            return None;
        }
        self.symbolize(ip)
    }

    fn symbolize(&self, ip: u64) -> Option<String> {
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

    fn display_symbol_name(&self, raw: &str, ip: u64) -> Option<String> {
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

fn is_compiler_local_symbol(name: &str) -> bool {
    let name = name.trim();
    // Covered target-local labels include LLVM/GNU `.L*` labels observed on
    // x86_64/riscv64/loongarch64 and ARM/AArch64 mapping symbols such as
    // `$x`/`$d`; all are rendering noise for host backtrace output.
    name.starts_with(".L") || name.starts_with('$')
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use object::{Object, ObjectSymbol};

    use super::*;

    #[unsafe(no_mangle)]
    extern "C" fn bt_symbolize_probe() {
        std::hint::black_box(());
    }

    #[test]
    fn infer_kind_filter_from_case_name() {
        assert_eq!(
            infer_kind_filter("backtrace-raw-normal", &[]).as_deref(),
            Some("raw")
        );
        assert_eq!(
            infer_kind_filter("foo-panic-bar", &[]).as_deref(),
            Some("panic")
        );
        assert_eq!(
            infer_kind_filter("my-trap-test", &[]).as_deref(),
            Some("trap")
        );
        assert_eq!(infer_kind_filter("draw-something", &[]), None);
        assert_eq!(infer_kind_filter("fs/shell", &[]), None);
        assert_eq!(infer_kind_filter("ipi", &[]), None);
        let blocks = parse_blocks(
            "BACKTRACE_BEGIN kind=panic arch=x86_64\nBT 0 ip=0x1 fp=0x2\nBACKTRACE_END\n",
        )
        .unwrap();
        assert_eq!(
            infer_kind_filter("generic", &blocks).as_deref(),
            Some("panic")
        );
    }

    #[test]
    fn parse_blocks_extracts_frames_with_prefix_noise() {
        let text = r#"
[0.000] INFO something
[0.001] BACKTRACE_BEGIN kind=panic arch=x86_64 alloc=true dwarf=false
[0.001] BT 0 ip=0x1000 fp=0x2000
[0.001] BT 1 ip=0x1010 fp=0x2010
[0.002] BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[0].arch.as_deref(), Some("x86_64"));
        assert_eq!(blocks[0].frames.len(), 2);
        assert_eq!(blocks[0].frames[0].idx, 0);
        assert_eq!(blocks[0].frames[0].ip, 0x1000);
        assert_eq!(blocks[0].frames[0].fp, Some(0x2000));
    }

    #[test]
    fn parse_blocks_accepts_missing_end_marker() {
        let text = r#"
BACKTRACE_BEGIN kind=trap arch=riscv64
BT 0 ip=0xdead fp=0xbeef
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "trap");
        assert_eq!(blocks[0].frames.len(), 1);
    }

    #[test]
    fn parse_blocks_splits_blocks_when_begin_repeats() {
        let text = r#"
BACKTRACE_BEGIN kind=panic arch=x86_64
BT 0 ip=0x1000 fp=0x2000
BACKTRACE_BEGIN kind=trap arch=x86_64
BT 0 ip=0x3000 fp=0x4000
BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[1].kind, "trap");
    }

    #[test]
    fn parse_blocks_captures_bt_error() {
        let text = r#"
BACKTRACE_BEGIN kind=panic arch=aarch64 alloc=false dwarf=false
BT_ERROR requires_alloc
BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[0].errors, vec!["requires_alloc".to_string()]);
        assert!(blocks[0].frames.is_empty());
    }

    #[test]
    fn parse_blocks_accepts_missing_fp() {
        let text = r#"
BACKTRACE_BEGIN kind=trap arch=riscv64
BT 0 ip=0xdead
BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].frames.len(), 1);
        assert_eq!(blocks[0].frames[0].ip, 0xdead);
        assert_eq!(blocks[0].frames[0].fp, None);
    }

    #[test]
    fn cli_accepts_adjust_ip_false() {
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = TestCli::try_parse_from([
            "tg-xtask",
            "symbolize",
            "--elf",
            "/tmp/fake.elf",
            "--adjust-ip",
            "false",
        ])
        .unwrap();

        let Command::Symbolize(args) = cli.command;
        assert!(!args.adjust_ip);
    }

    #[test]
    fn symbolize_resolves_symbol_with_ip_bias_under_aslr() {
        let exe = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&exe).unwrap();
        let obj = object::File::parse(bytes.as_slice()).unwrap();

        let runtime_ip = bt_symbolize_probe as *const () as usize as u64;
        let mut file_ip = None;
        for sym in obj.symbols() {
            let Ok(name) = sym.name() else {
                continue;
            };
            if name == "bt_symbolize_probe" || name == "_bt_symbolize_probe" {
                file_ip = Some(sym.address());
                break;
            }
        }
        let file_ip = file_ip.expect("failed to find bt_symbolize_probe symbol in current exe");

        let bias = file_ip as i64 - runtime_ip as i64;
        let ip_for_file = runtime_ip.wrapping_add_signed(bias);

        let symbolizer = HostSymbolizer::new(&exe).unwrap();
        let sym = symbolizer.symbolize(ip_for_file).unwrap();
        assert!(sym.contains("bt_symbolize_probe"));
    }

    #[test]
    fn infer_kind_filter_prefers_raw_from_case_name() {
        let blocks =
            parse_blocks("BACKTRACE_BEGIN kind=panic\nBT 0 ip=0x1\nBACKTRACE_END\n").unwrap();
        assert_eq!(
            infer_kind_filter("backtrace-raw-normal", &blocks).as_deref(),
            Some("raw")
        );
    }

    #[test]
    fn infer_kind_filter_uses_single_block_kind() {
        let blocks =
            parse_blocks("BACKTRACE_BEGIN kind=trap\nBT 0 ip=0x1\nBACKTRACE_END\n").unwrap();
        assert_eq!(
            infer_kind_filter("other-case", &blocks).as_deref(),
            Some("trap")
        );
    }

    #[test]
    fn infer_kind_filter_returns_none_for_multiple_kinds() {
        let blocks = parse_blocks(
            r#"
BACKTRACE_BEGIN kind=panic
BT 0 ip=0x1
BACKTRACE_BEGIN kind=trap
BT 0 ip=0x2
BACKTRACE_END
"#,
        )
        .unwrap();
        assert_eq!(infer_kind_filter("mixed", &blocks), None);
    }

    #[test]
    fn std_test_elf_path_uses_release_profile() {
        let path = std_test_elf_path(
            Path::new("/ws"),
            "x86_64-unknown-none",
            "arceos-test-suit",
            false,
        );
        assert_eq!(
            path,
            PathBuf::from("/ws/target/x86_64-unknown-linux-musl/release/arceos-test-suit")
        );
    }

    #[test]
    fn std_test_elf_path_maps_arceos_none_target_to_std_target_dir() {
        let path = std_test_elf_path(
            Path::new("/ws"),
            "x86_64-unknown-none",
            "arceos-test-suit",
            false,
        );
        assert_eq!(
            path,
            PathBuf::from("/ws/target/x86_64-unknown-linux-musl/release/arceos-test-suit")
        );
    }

    #[test]
    fn symbolize_skips_zero_ip() {
        let exe = std::env::current_exe().unwrap();
        let symbolizer = HostSymbolizer::new(&exe).unwrap();
        assert!(symbolizer.maybe_symbolize(0).is_none());
    }

    #[test]
    fn write_symbolized_blocks_tolerates_adjustment_exceeding_ip() {
        let exe = std::env::current_exe().unwrap();
        let symbolizer = HostSymbolizer::new(&exe).unwrap();
        let blocks = parse_blocks(
            "BACKTRACE_BEGIN kind=raw arch=riscv64\nBT 0 ip=0x1 fp=0x2\nBACKTRACE_END\n",
        )
        .unwrap();

        let mut out = Vec::new();
        write_symbolized_blocks(&mut out, &symbolizer, &blocks, None, true, 0).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("BACKTRACE_BLOCK 0 kind=raw arch=riscv64"));
        assert!(out.contains("BT 0 ip=0x1 fp=0x2"));
    }

    #[test]
    fn compiler_local_symbols_are_not_display_names() {
        assert!(is_compiler_local_symbol(".Lpcrel_hi31487"));
        assert!(is_compiler_local_symbol(".L0"));
        assert!(is_compiler_local_symbol(".Ltmp142"));
        assert!(is_compiler_local_symbol("$x"));
        assert!(is_compiler_local_symbol("$d"));
        assert!(!is_compiler_local_symbol(
            "starry_memtrack_sample_hard_leaf"
        ));
        assert!(!is_compiler_local_symbol(
            "_RNvNtNtNtCs66o47AdWPbf_13starry_kernel8pseudofs3dev8memtrack29record_hard_sample_allocation"
        ));
    }

    #[test]
    fn local_symbol_names_fall_back_to_nearest_text_symbol() {
        let exe = std::env::current_exe().unwrap();
        let mut symbolizer = HostSymbolizer::new(&exe).unwrap();
        symbolizer.text_symbols = vec![
            TextSymbol {
                address: 0x1000,
                size: 0x40,
                name: "starry_memtrack_sample_hard_mid".to_string(),
            },
            TextSymbol {
                address: 0x1040,
                size: 0x80,
                name: "starry_memtrack_sample_hard_leaf".to_string(),
            },
        ];

        assert_eq!(
            symbolizer.display_symbol_name(".Lpcrel_hi31487", 0x1060),
            Some("starry_memtrack_sample_hard_leaf".to_string())
        );
        assert_eq!(
            symbolizer.display_symbol_name(".Lpcrel_hi31487", 0x2000),
            None
        );
    }

    #[test]
    fn block_capture_writes_only_complete_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
        capture
            .push_bytes(
                b"[0.000] noise before\n\
[0.001] BACKTRACE_BEGIN kind=raw arch=x86_64\n\
[0.001] BT 0 ip=0x1000 fp=0x2000\n\
[0.002] BACKTRACE_END\n\
[0.003] more noise\n",
            )
            .unwrap();
        capture.finish().unwrap();

        let text = fs::read_to_string(&log_path).unwrap();
        assert!(!text.contains("noise"));
        assert!(text.contains("BACKTRACE_BEGIN kind=raw"));
        assert!(text.contains("BT 0 ip=0x1000"));
        assert!(text.contains("BACKTRACE_END"));

        let blocks = parse_blocks(&text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "raw");
        assert_eq!(blocks[0].frames.len(), 1);
    }

    #[test]
    fn block_capture_tee_suppresses_raw_blocks_on_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
        let guest = b"[0.000] boot line\n\
[0.001] BACKTRACE_BEGIN kind=raw arch=x86_64\n\
[0.001] BT 0 ip=0x1000 fp=0x2000\n\
[0.002] BACKTRACE_END\n\
[0.003] after block\n";
        let terminal = capture.push_bytes_for_tee(guest, true).unwrap();
        capture.finish().unwrap();

        let terminal = String::from_utf8(terminal).unwrap();
        assert!(terminal.contains("boot line"));
        assert!(terminal.contains("after block"));
        assert!(!terminal.contains("BACKTRACE_BEGIN"));
        assert!(!terminal.contains("BT 0 ip="));
        assert!(!terminal.contains("BACKTRACE_END"));

        let log = fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("BACKTRACE_BEGIN kind=raw"));
        assert!(log.contains("BT 0 ip=0x1000"));
    }

    #[test]
    fn block_capture_tee_forwards_all_bytes_when_not_suppressing() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
        let guest = b"BACKTRACE_BEGIN kind=raw\nBT 0 ip=0x1\nBACKTRACE_END\n";
        let terminal = capture.push_bytes_for_tee(guest, false).unwrap();
        assert_eq!(terminal, guest);
    }

    #[test]
    fn block_capture_splits_on_repeated_begin() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
        capture
            .push_bytes(
                b"BACKTRACE_BEGIN kind=panic arch=x86_64\n\
BT 0 ip=0x1 fp=0x2\n\
BACKTRACE_BEGIN kind=trap arch=x86_64\n\
BT 0 ip=0x3 fp=0x4\n\
BACKTRACE_END\n",
            )
            .unwrap();
        capture.finish().unwrap();

        let text = fs::read_to_string(&log_path).unwrap();
        let blocks = parse_blocks(&text).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[1].kind, "trap");
    }

    #[test]
    fn block_capture_accepts_bt_error_block() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let mut capture = BacktraceBlockCapture::create(Some(&log_path), None).unwrap();
        capture
            .push_bytes(
                b"BACKTRACE_BEGIN kind=panic arch=aarch64\n\
BT_ERROR requires_alloc\n\
BACKTRACE_END\n",
            )
            .unwrap();
        capture.finish().unwrap();

        let text = fs::read_to_string(&log_path).unwrap();
        let blocks = parse_blocks(&text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].errors, vec!["requires_alloc".to_string()]);
    }

    #[test]
    fn write_raw_blocks_from_output_filters_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("filtered.log");
        let transcript = "boot log\nBACKTRACE_BEGIN kind=raw\nBT 0 ip=0x10\nBACKTRACE_END\n";
        write_raw_blocks_from_output(transcript, &log_path).unwrap();
        let text = fs::read_to_string(&log_path).unwrap();
        assert!(!text.contains("boot log"));
        assert!(text.contains("BACKTRACE_BEGIN"));
    }

    #[test]
    fn should_persist_qemu_capture_log_on_keep_or_failure() {
        assert!(should_persist_qemu_capture_log(
            true,
            SymbolizeAfterQemuOutcome::Symbolized,
            true
        ));
        assert!(should_persist_qemu_capture_log(
            false,
            SymbolizeAfterQemuOutcome::Failed,
            true
        ));
        assert!(!should_persist_qemu_capture_log(
            false,
            SymbolizeAfterQemuOutcome::Symbolized,
            true
        ));
        assert!(!should_persist_qemu_capture_log(
            false,
            SymbolizeAfterQemuOutcome::Symbolized,
            false
        ));
    }

    #[test]
    fn block_capture_memory_only_skips_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let pending = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut capture = BacktraceBlockCapture::create(None, Some(pending.clone())).unwrap();
        capture
            .push_bytes(
                b"BACKTRACE_BEGIN kind=raw arch=x86_64\n\
BT 0 ip=0x1000 fp=0x2000\n\
BACKTRACE_END\n",
            )
            .unwrap();
        capture.finish().unwrap();
        assert!(!log_path.is_file());
        let blocks = pending.lock().unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0][0].contains("BACKTRACE_BEGIN"));
    }

    #[test]
    fn write_captured_blocks_to_log_writes_raw_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        let blocks = vec![vec![
            "BACKTRACE_BEGIN kind=raw".to_string(),
            "BT 0 ip=0x10".to_string(),
            "BACKTRACE_END".to_string(),
        ]];
        write_captured_blocks_to_log(&log_path, &blocks).unwrap();
        let text = fs::read_to_string(&log_path).unwrap();
        assert!(text.contains("BACKTRACE_BEGIN"));
        assert!(!text.contains("boot"));
    }

    #[test]
    fn should_delete_qemu_log_only_after_success_without_keep() {
        assert!(should_delete_qemu_log_after_symbolize(
            SymbolizeAfterQemuOutcome::Symbolized,
            false
        ));
        assert!(!should_delete_qemu_log_after_symbolize(
            SymbolizeAfterQemuOutcome::Symbolized,
            true
        ));
        assert!(!should_delete_qemu_log_after_symbolize(
            SymbolizeAfterQemuOutcome::Failed,
            false
        ));
        assert!(!should_delete_qemu_log_after_symbolize(
            SymbolizeAfterQemuOutcome::Skipped,
            false
        ));
    }

    #[test]
    fn apply_qemu_log_retention_removes_file_on_symbolized() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        fs::write(&log_path, "BACKTRACE_BEGIN kind=raw\nBACKTRACE_END\n").unwrap();
        apply_qemu_log_retention(&log_path, SymbolizeAfterQemuOutcome::Symbolized, false).unwrap();
        assert!(!log_path.is_file());
    }

    #[test]
    fn apply_qemu_log_retention_keeps_file_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        fs::write(&log_path, "BACKTRACE_BEGIN kind=raw\nBACKTRACE_END\n").unwrap();
        apply_qemu_log_retention(&log_path, SymbolizeAfterQemuOutcome::Symbolized, true).unwrap();
        assert!(log_path.is_file());
    }

    #[test]
    fn apply_qemu_log_retention_keeps_file_on_failed_symbolize() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        fs::write(&log_path, "truncated BACKTRACE_BEGIN\n").unwrap();
        apply_qemu_log_retention(&log_path, SymbolizeAfterQemuOutcome::Failed, false).unwrap();
        assert!(log_path.is_file());
    }

    #[test]
    fn maybe_symbolize_after_qemu_keeps_log_when_elf_missing() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        let elf_path = dir.path().join("missing.elf");
        fs::write(
            &log_path,
            "BACKTRACE_BEGIN kind=raw arch=x86_64\nBT 0 ip=0x1000\nBACKTRACE_END\n",
        )
        .unwrap();
        let outcome = maybe_symbolize_after_qemu(
            &elf_path,
            &log_path,
            "backtrace-raw-normal",
            false,
            None,
            None,
        )
        .unwrap();
        assert_eq!(outcome, SymbolizeAfterQemuOutcome::Failed);
        assert!(log_path.is_file());
    }

    #[test]
    fn stream_session_symbolizes_on_block_end() {
        let exe = std::env::current_exe().unwrap();
        let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
        session.on_block_complete(&[
            "[0.001] BACKTRACE_BEGIN kind=raw arch=x86_64".to_string(),
            "[0.001] BT 0 ip=0x1000 fp=0x2000".to_string(),
            "[0.002] BACKTRACE_END".to_string(),
        ]);
        assert!(session.streamed_symbolized());
        assert!(!session.streamed_failed());
    }

    #[test]
    fn maybe_symbolize_after_qemu_skips_reread_when_stream_ok() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        let exe = std::env::current_exe().unwrap();
        let memory_blocks = vec![vec![
            "BACKTRACE_BEGIN kind=raw arch=x86_64".to_string(),
            "BT 0 ip=0x1000".to_string(),
            "BACKTRACE_END".to_string(),
        ]];
        let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
        session.on_block_complete(&memory_blocks[0]);
        let outcome = maybe_symbolize_after_qemu(
            &exe,
            &log_path,
            "backtrace-raw-normal",
            false,
            Some(&session),
            Some(&memory_blocks),
        )
        .unwrap();
        assert_eq!(outcome, SymbolizeAfterQemuOutcome::Symbolized);
        assert!(!log_path.is_file());
    }

    #[test]
    fn maybe_symbolize_after_qemu_writes_log_when_keep_requested() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("qemu.log");
        let exe = std::env::current_exe().unwrap();
        let memory_blocks = vec![vec![
            "BACKTRACE_BEGIN kind=raw arch=x86_64".to_string(),
            "BT 0 ip=0x1000".to_string(),
            "BACKTRACE_END".to_string(),
        ]];
        let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
        session.on_block_complete(&memory_blocks[0]);
        let outcome = maybe_symbolize_after_qemu(
            &exe,
            &log_path,
            "backtrace-raw-normal",
            true,
            Some(&session),
            Some(&memory_blocks),
        )
        .unwrap();
        assert_eq!(outcome, SymbolizeAfterQemuOutcome::Symbolized);
        assert!(log_path.is_file());
        let text = fs::read_to_string(&log_path).unwrap();
        assert!(text.contains("BACKTRACE_BEGIN"));
    }

    #[test]
    fn block_capture_queues_stream_blocks_for_symbolize() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("blocks.log");
        let exe = std::env::current_exe().unwrap();
        let session = BacktraceSymbolizeSession::try_new(&exe, "backtrace-raw-normal").unwrap();
        let pending = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut capture =
            BacktraceBlockCapture::create(Some(&log_path), Some(pending.clone())).unwrap();
        capture
            .push_bytes(
                b"BACKTRACE_BEGIN kind=raw arch=x86_64\n\
BT 0 ip=0x1000 fp=0x2000\n\
BACKTRACE_END\n",
            )
            .unwrap();
        capture.finish().unwrap();
        flush_pending_stream_symbolize(&session, &pending);
        assert!(session.streamed_symbolized());
    }
}
