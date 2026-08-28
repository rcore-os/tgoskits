//! See Linux Documentation for details: <https://docs.kernel.org/trace/ftrace.html>
mod control;
mod registry;
mod sched;
mod trace;
mod trace_pipe;

use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    num::NonZero,
    ops::Deref,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_runtime::hal::{percpu::this_cpu_id, time::monotonic_time_nanos};
use ax_task::{IrqNotify, current};
use ax_tracepoint::*;
use axfs_ng_vfs::NodePermission;
use axpoll::{IoEvents, PollSet};
pub use registry::KernelExtTracePoint;

use crate::{
    StarryError, StarryResult,
    pseudofs::{DirMaker, DirMapping, SeqObject, SimpleDir, SimpleFs, SpecialFsFile},
    sync::Mutex,
    task::{AsThread, PidIdentityId, TgidNumber},
};

/// Maximum number of trace records kept in the raw trace pipe ring buffer.
const TRACE_RAW_PIPE_CAPACITY: usize = 4096;
/// Maximum number of PID→cmdline entries in the command-line cache.
const TRACE_CMDLINE_CACHE_SIZE: usize = 4096;

/// Look up a registered tracepoint by its numeric id (as found in
/// `/sys/kernel/debug/tracing/events/<subsystem>/<event>/id`).
///
/// Returns `None` if the id is unknown or the registry has not been
/// initialized yet.
pub fn lookup_ext_tracepoint(id: u32) -> Option<KernelExtTracePoint> {
    TRACE_STATE.ext_tracepoints.get()?.get(&id).cloned()
}

/// Find a registered tracepoint by name. Returns the first `ExtTracePoint`
/// whose underlying `TracePoint`'s name matches `name`.
///
/// Returns `None` if no tracepoint matches or the registry has not been
/// initialized yet.
pub fn find_ext_tracepoint_by_name(name: &str) -> Option<KernelExtTracePoint> {
    for ext_tp in TRACE_STATE.ext_tracepoints.get()?.values() {
        if ext_tp.trace_point().name() == name {
            return Some(ext_tp.clone());
        }
    }
    None
}

struct TraceState {
    point_map: LazyInit<TracePointMap<KernelTraceAux>>,
    raw_pipe: Mutex<IdentityTracePipe>,
    pipe_event: PollSet,
    pipe_notify: IrqNotify,
    cmdline_cache: LazyInit<Mutex<TraceCmdLineCache>>,
    ext_tracepoints: LazyInit<BTreeMap<u32, KernelExtTracePoint>>,
}

impl TraceState {
    const fn new() -> Self {
        Self {
            point_map: LazyInit::new(),
            raw_pipe: Mutex::new(IdentityTracePipe::new(TRACE_RAW_PIPE_CAPACITY)),
            pipe_event: PollSet::new(),
            pipe_notify: IrqNotify::new(),
            cmdline_cache: LazyInit::new(),
            ext_tracepoints: LazyInit::new(),
        }
    }
}

static TRACE_STATE: TraceState = TraceState::new();
static TRACE_PIPE_NOTIFY_WORKER: AtomicBool = AtomicBool::new(false);

/// One trace record with the stable task generation and comm captured at emit time.
#[derive(Clone)]
struct IdentityTraceRecord {
    record: TracePipeRecord,
    identity_id: PidIdentityId,
    tgid: TgidNumber,
    comm: String,
}

impl IdentityTraceRecord {
    fn new(
        timestamp: u64,
        cpu_id: u32,
        event: Vec<u8>,
        identity_id: PidIdentityId,
        tgid: TgidNumber,
        comm: String,
    ) -> Self {
        Self {
            record: TracePipeRecord::new(timestamp, cpu_id, event),
            identity_id,
            tgid,
            comm,
        }
    }
}

trait IdentityTraceBuffer {
    fn peek(&self) -> Option<&IdentityTraceRecord>;
    fn pop(&mut self) -> Option<IdentityTraceRecord>;
    fn is_empty(&self) -> bool;
}

struct IdentityTracePipe {
    capacity: usize,
    records: Vec<IdentityTraceRecord>,
}

impl IdentityTracePipe {
    const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            records: Vec::new(),
        }
    }

    fn push(&mut self, record: IdentityTraceRecord) {
        if self.capacity == 0 {
            return;
        }
        if self.records.len() == self.capacity {
            self.records.remove(0);
        }
        self.records.push(record);
    }

    fn clear(&mut self) {
        self.records.clear();
    }

    fn snapshot(&self) -> IdentityTraceSnapshot {
        IdentityTraceSnapshot(self.records.clone())
    }
}

impl IdentityTraceBuffer for IdentityTracePipe {
    fn peek(&self) -> Option<&IdentityTraceRecord> {
        self.records.first()
    }

    fn pop(&mut self) -> Option<IdentityTraceRecord> {
        (!self.records.is_empty()).then(|| self.records.remove(0))
    }

    fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

struct IdentityTraceSnapshot(Vec<IdentityTraceRecord>);

impl IdentityTraceSnapshot {
    fn default_fmt_str(&self) -> String {
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

impl IdentityTraceBuffer for IdentityTraceSnapshot {
    fn peek(&self) -> Option<&IdentityTraceRecord> {
        self.0.first()
    }

    fn pop(&mut self) -> Option<IdentityTraceRecord> {
        (!self.0.is_empty()).then(|| self.0.remove(0))
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn current_comm() -> String {
    let curr = current();
    let exe_path = curr.as_thread().proc_data.exe_path.read();
    exe_path
        .split(' ')
        .next()
        .unwrap_or("unknown")
        .split('/')
        .next_back()
        .unwrap_or("unknown")
        .to_string()
}

pub struct KernelTraceAux;

impl KernelTraceOps for KernelTraceAux {
    fn current_pid() -> u32 {
        let curr = current();
        let proc_data = &curr.as_thread().proc_data;
        proc_data.proc.pid().get()
    }

    fn trace_pipe_push_raw_record(buf: &[u8]) {
        let curr = current();
        let identity = curr.as_thread().proc_data.identity();
        TRACE_STATE.raw_pipe.lock().push(IdentityTraceRecord::new(
            monotonic_time_nanos(),
            this_cpu_id() as _,
            buf.to_vec(),
            identity.id(),
            curr.as_thread().proc_data.proc.pid_number(),
            current_comm(),
        ));
        TRACE_STATE.pipe_notify.notify_irq();
    }

    fn trace_cmdline_push(external_pid: u32) {
        let tgid = current().as_thread().proc_data.proc.pid();
        debug_assert_eq!(external_pid, tgid.get());
        // `saved_cmdlines` is an external PID-number ABI and therefore keeps
        // its numeric key. Historical trace records never consult this cache:
        // they carry their own generation and emission-time command name.
        TRACE_STATE
            .cmdline_cache
            .lock()
            .insert(tgid.get(), &current_comm());
    }

    fn read_tracepoint_state<R>(id: u32, f: impl FnOnce(&ExtTracePoint<Self>) -> R) -> R {
        let ext_tp = TRACE_STATE
            .ext_tracepoints
            .deref()
            .get(&id)
            .expect("Tracepoint not found");
        ext_tp.read(f)
    }

    fn write_tracepoint_state<R>(id: u32, f: impl FnOnce(&mut ExtTracePoint<Self>) -> R) -> R {
        let ext_tp = TRACE_STATE
            .ext_tracepoints
            .deref()
            .get(&id)
            .expect("Tracepoint not found");
        ext_tp.update(f)
    }
}

fn start_trace_pipe_notify_worker() {
    if TRACE_PIPE_NOTIFY_WORKER.swap(true, Ordering::AcqRel) {
        return;
    }
    ax_task::spawn_with_name(
        || loop {
            TRACE_STATE.pipe_notify.wait();
            // Trace records are queued before the deferred poll wake.
            unsafe { TRACE_STATE.pipe_event.wake(IoEvents::IN) };
        },
        "trace-pipe-notify".into(),
    );
}

/// Carries the unread suffix of a formatted text record across `read_at` calls.
///
/// Tracefs text records are consumed as whole records from the backing trace
/// buffer, but the user-provided read buffer may be smaller than one formatted
/// line. This helper lets callers return the prefix immediately and keep the
/// suffix for later reads, avoiding a false EOF when `buf` is too small.
struct TextDrain {
    pending: Vec<u8>,
    pos: usize,
}

impl TextDrain {
    /// Creates an empty text drain with no pending bytes.
    const fn new() -> Self {
        Self {
            pending: Vec::new(),
            pos: 0,
        }
    }

    /// Discards any pending bytes and returns the drain to the initial state.
    fn reset(&mut self) {
        self.pending.clear();
        self.pos = 0;
    }

    /// Copies as many pending bytes as possible into `buf`.
    ///
    /// Returns the number of bytes copied. If all pending bytes are drained,
    /// the internal state is reset so the next read can consume a new record.
    fn drain_pending(&mut self, buf: &mut [u8]) -> usize {
        if self.pending.is_empty() {
            return 0;
        }

        let remaining = &self.pending[self.pos..];
        let len = remaining.len().min(buf.len());
        buf[..len].copy_from_slice(&remaining[..len]);
        self.pos += len;

        if self.pos == self.pending.len() {
            self.reset();
        }
        len
    }

    /// Copies one formatted record into `buf` starting at `copy_len`.
    ///
    /// Returns `false` when `buf` has no remaining space and the caller should
    /// stop without consuming a new backing record. If only a prefix fits, the
    /// remaining suffix is stored internally and the method returns `true`, so
    /// the caller may consume the backing record.
    fn copy_record(&mut self, record: &[u8], buf: &mut [u8], copy_len: &mut usize) -> bool {
        if record.is_empty() {
            return true;
        }

        let remaining = buf.len() - *copy_len;
        if remaining == 0 {
            return false;
        }

        let len = record.len().min(remaining);
        buf[*copy_len..*copy_len + len].copy_from_slice(&record[..len]);
        *copy_len += len;

        if len < record.len() {
            self.pending.extend_from_slice(&record[len..]);
        }
        true
    }
}

fn common_trace_pipe_read(
    trace_buf: &mut dyn IdentityTraceBuffer,
    drain: &mut TextDrain,
    buf: &mut [u8],
) -> usize {
    let mut copy_len = drain.drain_pending(buf);
    if copy_len == buf.len() {
        return copy_len;
    }

    loop {
        if let Some(record) = trace_buf.peek() {
            debug_assert_ne!(record.identity_id.get(), 0);
            let mut record_cmdline = TraceCmdLineCache::new(NonZero::new(1).unwrap());
            record_cmdline.insert(record.tgid.get(), &record.comm);
            let record_str = match TraceEntryParser::parse::<KernelTraceAux>(
                &TRACE_STATE.point_map,
                &record_cmdline,
                &record.record,
            ) {
                Ok(record) => record,
                Err(error) => {
                    warn!("discarding invalid trace record: {error}");
                    trace_buf.pop();
                    continue;
                }
            };
            if !drain.copy_record(record_str.as_bytes(), buf, &mut copy_len) {
                break;
            }
            trace_buf.pop(); // Remove the record after reading

            if copy_len == buf.len() {
                break;
            }
            continue;
        }
        break;
    }
    copy_len
}

/// Initialize registered tracepoints. This should be called after static keys are initialized, and before any tracepoint is hit.
pub fn tracepoint_init() -> StarryResult<()> {
    let (tp_map, ext_tps) =
        global_init_events::<KernelTraceAux>().map_err(|_| StarryError::InvalidInput)?;

    let ext_tps = ext_tps
        .into_iter()
        .map(|ext_tp| (ext_tp.trace_point().id(), KernelExtTracePoint::new(ext_tp)))
        .collect::<BTreeMap<_, _>>();

    ax_println!("Initialized {} tracepoints", tp_map.len());
    TRACE_STATE.point_map.init_once(tp_map);
    TRACE_STATE.ext_tracepoints.init_once(ext_tps);
    TRACE_STATE
        .cmdline_cache
        .init_once(Mutex::new(TraceCmdLineCache::new(
            NonZero::new(TRACE_CMDLINE_CACHE_SIZE).unwrap(),
        )));
    start_trace_pipe_notify_worker();
    Ok(())
}

/// Initialize events directory in debugfs
fn init_events(fs: Arc<SimpleFs>) -> DirMaker {
    let mut events_root = DirMapping::new();
    let mut subsystem = BTreeMap::new();

    for ext_tp in TRACE_STATE.ext_tracepoints.deref().values() {
        let tp = ext_tp.trace_point();
        let subsystem_name = tp.system();
        let event_name = tp.name();

        let subsystem_root = {
            if !subsystem.contains_key(subsystem_name) {
                let new_root = DirMapping::new();
                subsystem.insert(subsystem_name.to_string(), new_root);
            }
            subsystem.get_mut(subsystem_name).unwrap()
        };

        let mut event_root = DirMapping::new();
        event_root.add(
            "enable",
            SpecialFsFile::new_regular_with_perm(
                fs.clone(),
                control::EventEnableObj::new(ext_tp.clone()),
                NodePermission::from_bits_truncate(0o640),
            ),
        );
        event_root.add("format", {
            let seq_obj = SeqObject::new({
                let format_file = TracePointFormatFile::new(tp);
                move || Ok(format_file.read())
            });
            SpecialFsFile::new_regular_with_perm(
                fs.clone(),
                seq_obj,
                NodePermission::from_bits_truncate(0o440),
            )
        });

        event_root.add("id", {
            let seq_obj = SeqObject::new({
                let id_file = TracePointIdFile::new(tp);
                move || Ok(id_file.read())
            });
            SpecialFsFile::new_regular_with_perm(
                fs.clone(),
                seq_obj,
                NodePermission::from_bits_truncate(0o440),
            )
        });
        event_root.add(
            "filter",
            SpecialFsFile::new_regular_with_perm(
                fs.clone(),
                control::EventFilterObj::new(ext_tp.clone()),
                NodePermission::from_bits_truncate(0o640),
            ),
        );
        subsystem_root.add(
            event_name,
            SimpleDir::new_maker(fs.clone(), Arc::new(event_root)),
        );
    }
    for (subsystem_name, subsystem_root) in subsystem {
        events_root.add(
            &subsystem_name,
            SimpleDir::new_maker(fs.clone(), Arc::new(subsystem_root)),
        );
    }
    SimpleDir::new_maker(fs, Arc::new(events_root))
}

/// Initialize tracing directory in debugfs
pub fn init_tracing_dir(fs: Arc<SimpleFs>) -> DirMaker {
    let mut tracing_root = DirMapping::new();
    tracing_root.set_cacheable(false);

    tracing_root.add(
        "saved_cmdlines_size",
        SpecialFsFile::new_regular_with_perm(
            fs.clone(),
            control::TraceCmdLineSizeObj,
            NodePermission::from_bits_truncate(0o640),
        ),
    );
    tracing_root.add(
        "trace_pipe",
        SpecialFsFile::new_regular_with_perm(
            fs.clone(),
            trace_pipe::TracePipeFile::new(),
            NodePermission::from_bits_truncate(0o440),
        ),
    );
    tracing_root.add_dynamic("saved_cmdlines", {
        let fs = fs.clone();
        move || {
            SpecialFsFile::new_regular_with_perm(
                fs.clone(),
                trace::TraceCmdLineFile::new(),
                NodePermission::from_bits_truncate(0o440),
            )
            .into()
        }
    });
    tracing_root.add_dynamic("trace", {
        let fs = fs.clone();
        move || {
            SpecialFsFile::new_regular_with_perm(
                fs.clone(),
                trace::TraceFile::new(),
                NodePermission::from_bits_truncate(0o640),
            )
            .into()
        }
    });
    tracing_root.add("events", init_events(fs.clone()));
    SimpleDir::new_maker(fs, Arc::new(tracing_root))
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn reused_pid_keeps_each_records_emission_generation_and_comm() {
        let tgid = TgidNumber::try_from(7).unwrap();
        let first_generation = PidIdentityId::try_from(1001).unwrap();
        let second_generation = PidIdentityId::try_from(1002).unwrap();
        let mut pipe = IdentityTracePipe::new(2);

        pipe.push(IdentityTraceRecord::new(
            1,
            0,
            Vec::new(),
            first_generation,
            tgid,
            "first".to_string(),
        ));
        pipe.push(IdentityTraceRecord::new(
            2,
            0,
            Vec::new(),
            second_generation,
            tgid,
            "second".to_string(),
        ));

        let first = pipe.pop().unwrap();
        let second = pipe.pop().unwrap();
        assert_eq!(first.identity_id, first_generation);
        assert_eq!(first.comm, "first");
        assert_eq!(second.identity_id, second_generation);
        assert_eq!(second.comm, "second");
    }
}
