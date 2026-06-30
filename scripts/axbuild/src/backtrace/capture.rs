use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use super::symbolize::BacktraceSymbolizeSession;

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

/// Incremental state machine: captures `BACKTRACE_BEGIN` ... `BACKTRACE_END` blocks to memory
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
