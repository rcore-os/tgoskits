use ax_sync::Mutex;
use axfs_ng_vfs::VfsResult;
use ktracepoint::{TraceCmdLineCacheSnapshot, TracePipeSnapshot};

use crate::pseudofs::DirectRwFsFileOps;

/// File representing the trace content.
pub struct TraceFile(Mutex<TraceFileState>);

struct TraceFileState {
    snapshot: Option<TracePipeSnapshot>,
    drain: super::TextDrain,
}

impl TraceFileState {
    const fn new() -> Self {
        Self {
            snapshot: None,
            drain: super::TextDrain::new(),
        }
    }

    fn reset(&mut self, snapshot: TracePipeSnapshot) {
        self.snapshot = Some(snapshot);
        self.drain.reset();
    }
}

impl TraceFile {
    /// Creates a new `TraceFile` instance.
    pub const fn new() -> Self {
        TraceFile(Mutex::new(TraceFileState::new()))
    }
}

impl DirectRwFsFileOps for TraceFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let offset = offset as usize;

        let mut state = self.0.lock();
        if state.snapshot.is_none() || offset == 0 {
            let snapshot = super::TRACE_STATE.raw_pipe.lock().snapshot();
            state.reset(snapshot);
        }

        let TraceFileState { snapshot, drain } = &mut *state;
        let snapshot = snapshot.as_mut().unwrap();

        let default_fmt_str = snapshot.default_fmt_str();
        if offset >= default_fmt_str.len() {
            Ok(super::common_trace_pipe_read(snapshot, drain, buf))
        } else {
            let len = buf.len().min(default_fmt_str.len() - offset);
            buf[..len].copy_from_slice(&default_fmt_str.as_bytes()[offset..offset + len]);
            Ok(len)
        }
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let mut state = self.0.lock();
        state.snapshot = None;
        state.drain.reset();
        let mut trace_raw_pipe = super::TRACE_STATE.raw_pipe.lock();
        trace_raw_pipe.clear();
        Ok(buf.len())
    }
}

/// File representing the trace command line cache.
pub struct TraceCmdLineFile(Mutex<TraceCmdLineFileState>);

struct TraceCmdLineFileState {
    snapshot: Option<TraceCmdLineCacheSnapshot>,
    drain: super::TextDrain,
}

impl TraceCmdLineFileState {
    const fn new() -> Self {
        Self {
            snapshot: None,
            drain: super::TextDrain::new(),
        }
    }

    fn reset(&mut self, snapshot: TraceCmdLineCacheSnapshot) {
        self.snapshot = Some(snapshot);
        self.drain.reset();
    }
}

impl TraceCmdLineFile {
    /// Creates a new `TraceCmdLineFile` instance.
    pub const fn new() -> Self {
        TraceCmdLineFile(Mutex::new(TraceCmdLineFileState::new()))
    }
}

impl DirectRwFsFileOps for TraceCmdLineFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let mut state = self.0.lock();
        if state.snapshot.is_none() || offset == 0 {
            let snapshot = super::TRACE_STATE.cmdline_cache.lock().snapshot();
            state.reset(snapshot);
        }

        let TraceCmdLineFileState { snapshot, drain } = &mut *state;

        let mut copy_len = drain.drain_pending(buf);
        if copy_len == buf.len() {
            return Ok(copy_len);
        }

        let snapshot = snapshot.as_mut().unwrap();
        loop {
            if let Some(record_str) = snapshot.peek() {
                if !drain.copy_record(record_str.as_bytes(), buf, &mut copy_len) {
                    break;
                }
                snapshot.pop(); // Remove the record after reading

                if copy_len == buf.len() {
                    break;
                }
                continue;
            }
            break; // No more records to read
        }
        Ok(copy_len)
    }
}
