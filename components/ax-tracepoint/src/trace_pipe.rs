use alloc::{format, string::String, vec::Vec};
use core::num::NonZero;

use lru::LruCache;

use crate::{KernelTraceOps, TraceEntry, TracePointMap};

/// An error produced while decoding a trace pipe record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TraceParseError {
    /// The record does not contain a complete common trace header.
    #[error("trace record is too short: expected at least {expected} bytes, got {actual}")]
    RecordTooShort {
        /// Minimum number of bytes required.
        expected: usize,
        /// Number of bytes supplied.
        actual: usize,
    },
    /// The record names an event ID that is absent from the tracepoint map.
    #[error("unknown tracepoint ID {id}")]
    UnknownTracepoint {
        /// Event ID read from the common trace header.
        id: u32,
    },
    /// The event-specific payload is shorter than its generated entry layout.
    #[error("trace payload is too short: expected at least {expected} bytes, got {actual}")]
    PayloadTooShort {
        /// Minimum number of bytes required.
        expected: usize,
        /// Number of bytes supplied.
        actual: usize,
    },
}

/// A trace pipe record with host-provided metadata captured when the event is written.
#[derive(Clone, Debug)]
pub struct TracePipeRecord {
    timestamp: u64,
    cpu_id: u32,
    event: Vec<u8>,
}

impl TracePipeRecord {
    /// Create a new trace pipe record.
    pub fn new(timestamp: u64, cpu_id: u32, event: Vec<u8>) -> Self {
        Self {
            timestamp,
            cpu_id,
            event,
        }
    }

    /// The event timestamp in nanoseconds.
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// The CPU ID on which the event was recorded.
    pub fn cpu_id(&self) -> u32 {
        self.cpu_id
    }

    /// The raw trace event payload.
    pub fn event(&self) -> &[u8] {
        &self.event
    }
}

/// A trait defining operations for a trace pipe buffer.
pub trait TracePipeOps {
    /// Returns the first record in the trace pipe buffer without removing it.
    fn peek(&self) -> Option<&TracePipeRecord>;

    /// Remove and return the first record in the trace pipe buffer.
    fn pop(&mut self) -> Option<TracePipeRecord>;

    /// Whether the trace pipe buffer is empty.
    fn is_empty(&self) -> bool;
}

/// A raw trace pipe buffer that stores trace events as byte vectors.
pub struct TracePipeRaw {
    max_record: usize,
    event_buf: Vec<TracePipeRecord>,
}

impl TracePipeRaw {
    /// Create a new TracePipeRaw with the specified maximum number of records.
    pub const fn new(max_record: usize) -> Self {
        Self {
            max_record,
            event_buf: Vec::new(),
        }
    }

    /// Set the maximum number of records to keep in the trace pipe buffer.
    ///
    /// If the current number of records exceeds this limit, the oldest records will be removed.
    pub fn set_max_record(&mut self, max_record: usize) {
        self.max_record = max_record;
        if self.event_buf.len() > max_record {
            let remove_count = self.event_buf.len() - max_record;
            self.event_buf.drain(0..remove_count);
        }
    }

    /// Push a new event into the trace pipe buffer without metadata.
    ///
    /// Prefer [`Self::push_record`] when the host can provide event-time metadata.
    pub fn push_event(&mut self, event: Vec<u8>) {
        self.push_record(0, 0, event);
    }

    /// Push a new event record into the trace pipe buffer.
    pub fn push_record(&mut self, timestamp: u64, cpu_id: u32, event: Vec<u8>) {
        if self.max_record == 0 {
            return;
        }
        if self.event_buf.len() >= self.max_record {
            self.event_buf.remove(0); // Remove the oldest record
        }
        self.event_buf
            .push(TracePipeRecord::new(timestamp, cpu_id, event));
    }

    /// The number of events currently in the trace pipe buffer.
    pub fn event_count(&self) -> usize {
        self.event_buf.len()
    }

    /// Clear the trace pipe buffer.
    pub fn clear(&mut self) {
        self.event_buf.clear();
    }

    /// Create a snapshot of the current state of the trace pipe buffer.
    pub fn snapshot(&self) -> TracePipeSnapshot {
        TracePipeSnapshot::new(self.event_buf.clone())
    }

    /// Get the maximum number of records allowed in the trace pipe buffer.
    pub fn max_record(&self) -> usize {
        self.max_record
    }
}

impl TracePipeOps for TracePipeRaw {
    fn peek(&self) -> Option<&TracePipeRecord> {
        self.event_buf.first()
    }

    fn pop(&mut self) -> Option<TracePipeRecord> {
        if self.event_buf.is_empty() {
            None
        } else {
            Some(self.event_buf.remove(0))
        }
    }

    fn is_empty(&self) -> bool {
        self.event_buf.is_empty()
    }
}

/// A snapshot of the trace pipe buffer at a specific point in time.
#[derive(Debug)]
pub struct TracePipeSnapshot(Vec<TracePipeRecord>);

impl TracePipeSnapshot {
    /// Create a new TracePipeSnapshot with the given event buffer.
    pub fn new(event_buf: Vec<TracePipeRecord>) -> Self {
        Self(event_buf)
    }

    /// The formatted string representation to be used as a header for the trace pipe output.
    pub fn default_fmt_str(&self) -> String {
        let show = "#
#
#                                _-----=> irqs-off/BH-disabled
#                               / _----=> need-resched
#                              | / _---=> hardirq/softirq
#                              || / _--=> preempt-depth
#                              ||| / _-=> migrate-disable
#                              |||| /     delay
#           TASK-PID     CPU#  |||||  TIMESTAMP  FUNCTION
#              | |         |   |||||     |         |
";
        format!(
            "# tracer: nop\n#\n# entries-in-buffer/entries-written: {}/{}   #P:32\n{}",
            self.0.len(),
            self.0.len(),
            show
        )
    }
}

impl TracePipeOps for TracePipeSnapshot {
    fn peek(&self) -> Option<&TracePipeRecord> {
        self.0.first()
    }

    fn pop(&mut self) -> Option<TracePipeRecord> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.remove(0))
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A cache for storing command line arguments for each trace point.
///
/// See <https://www.kernel.org/doc/Documentation/trace/ftrace.txt>
pub struct TraceCmdLineCache {
    // cmdline: Vec<(u32, [u8; 16])>,
    cmdline: LruCache<u32, String>,
}

impl TraceCmdLineCache {
    /// Create a new TraceCmdLineCache with the specified maximum number of records.
    pub fn new(max_record: NonZero<usize>) -> Self {
        Self {
            cmdline: LruCache::new(max_record),
        }
    }

    /// Insert a command line argument for a trace point.
    ///
    /// If the command line exceeds 16 bytes, it will be truncated.
    /// If the cache exceeds the maximum record limit, the oldest entry will be removed.
    pub fn insert(&mut self, id: u32, cmdline: &str) {
        const MAX_CMDLINE_LEN: usize = 16;
        let (cmdline, _) = cmdline.split_at(MAX_CMDLINE_LEN.min(cmdline.len()));
        let line = format!("{} {}\n", id, cmdline);
        self.cmdline.put(id, line);
    }

    /// Get the command line argument for a trace point.
    pub fn get(&self, id: u32) -> Option<&str> {
        self.cmdline
            .iter()
            .find(|(key, _)| **key == id)
            .and_then(|(_, value)| {
                let line = value.as_str();
                line.split_once(' ')
                    .map(|(_, command)| command.trim_end_matches('\n'))
            })
    }

    /// Set the maximum length for command line arguments.
    pub fn set_max_record(&mut self, max_len: NonZero<usize>) {
        self.cmdline.resize(max_len);
    }

    /// Get the maximum number of records in the cache.
    pub fn max_record(&self) -> usize {
        self.cmdline.cap().get()
    }

    /// Create a snapshot of the current state of the command line cache.
    pub fn snapshot(&self) -> TraceCmdLineCacheSnapshot {
        let cmdline = self
            .cmdline
            .iter()
            .map(|(_, value)| value)
            .cloned()
            .collect();
        TraceCmdLineCacheSnapshot::new(cmdline)
    }
}

/// A snapshot of the command line cache at a specific point in time.
#[derive(Debug)]
pub struct TraceCmdLineCacheSnapshot(Vec<String>);

impl TraceCmdLineCacheSnapshot {
    /// Create a new TraceCmdLineCacheSnapshot with the given command line entries.
    pub fn new(cmdline: Vec<String>) -> Self {
        Self(cmdline)
    }

    /// Return the first command line entry in the cache.
    pub fn peek(&self) -> Option<&String> {
        self.0.first()
    }

    /// Remove and return the first command line entry in the cache.
    pub fn pop(&mut self) -> Option<String> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.remove(0))
        }
    }
}

/// A parser for trace entries that formats them into human-readable strings.
pub struct TraceEntryParser;

impl TraceEntryParser {
    /// Parse the trace entry and return a formatted string.
    pub fn parse<K: KernelTraceOps>(
        tracepoint_map: &TracePointMap<K>,
        cmdline_cache: &TraceCmdLineCache,
        record: &TracePipeRecord,
    ) -> Result<String, TraceParseError> {
        let entry = record.event();
        let header_len = core::mem::size_of::<TraceEntry>();
        if entry.len() < header_len {
            return Err(TraceParseError::RecordTooShort {
                expected: header_len,
                actual: entry.len(),
            });
        }
        // SAFETY: the length check covers the complete C-layout header;
        // `TraceEntry` contains only integer fields, so every bit pattern is
        // valid, and `read_unaligned` does not require Vec<u8> alignment.
        let trace_entry = unsafe { core::ptr::read_unaligned(entry.as_ptr().cast::<TraceEntry>()) };
        let id = trace_entry.common_type as u32;
        let tracepoint = tracepoint_map
            .get(&id)
            .ok_or(TraceParseError::UnknownTracepoint { id })?;
        let fmt_func = tracepoint.fmt_func();
        let str = fmt_func(&entry[header_len..])?;

        let time = record.timestamp();
        let cpu_id = record.cpu_id();

        // Copy the packed field to a local variable to avoid unaligned reference
        let pid = trace_entry.common_pid;
        let pname = cmdline_cache
            .get(trace_entry.common_pid as u32)
            .unwrap_or("<...>");

        let secs = time / 1_000_000_000;
        let usec_rem = time % 1_000_000_000 / 1000;

        Ok(format!(
            "{:>16}-{:<7} [{:03}] {} {:5}.{:06}: {}({})\n",
            pname,
            pid,
            cpu_id,
            trace_entry.trace_print_lat_fmt(),
            secs,
            usec_rem,
            tracepoint.name(),
            str
        ))
    }
}
