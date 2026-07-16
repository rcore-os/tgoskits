//! See Linux Documentation for details: <https://docs.kernel.org/trace/ftrace.html>
mod control;
mod sched;
mod sched_filter;
mod trace;
mod trace_pipe;

use alloc::{collections::BTreeMap, string::ToString, sync::Arc, vec::Vec};
use core::{
    num::NonZero,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoPreempt;
use ax_lazyinit::LazyInit;
use ax_memory_addr::VirtAddr;
use ax_runtime::hal::{percpu::this_cpu_id, time::monotonic_time_nanos};
use ax_sync::SpinMutex;
use axfs_ng_vfs::NodePermission;
use axpoll::{IoEvents, PollSet};
use ktracepoint::*;

use crate::{
    pseudofs::{DirMaker, DirMapping, SeqObject, SimpleDir, SimpleFs, SpecialFsFile},
    task::{future::IrqNotify, try_current_user_irq_view},
};

/// Maximum number of trace records kept in the raw trace pipe ring buffer.
const TRACE_RAW_PIPE_CAPACITY: usize = 4096;
/// Maximum number of PID→cmdline entries in the command-line cache.
const TRACE_CMDLINE_CACHE_SIZE: usize = 4096;

// The registry entry is locked from the tracepoint fire path, which for
// `sched:sched_switch` runs from the ax-task switch path (IRQ off,
// preemption disabled). A sleeping `ax_sync::PiMutex` would trip the
// "sleeping in atomic context" guard there, so this lock must be a
// non-sleeping spinlock — the same kind the perf output path (`PERF_FILE`)
// uses for exactly this reason.
pub type KernelExtTracePoint = Arc<SpinNoPreempt<ExtTracePoint<KernelTraceAux>>>;

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
        if ext_tp.lock().trace_point().name() == name {
            return Some(ext_tp.clone());
        }
    }
    None
}

struct TraceState {
    point_map: LazyInit<TracePointMap<KernelTraceAux>>,
    raw_pipe: SpinMutex<TracePipeRaw>,
    pipe_event: PollSet,
    pipe_notify: IrqNotify,
    sched_notify: IrqNotify,
    cmdline_cache: LazyInit<SpinMutex<TraceCmdLineCache>>,
    ext_tracepoints: LazyInit<BTreeMap<u32, KernelExtTracePoint>>,
}

impl TraceState {
    const fn new() -> Self {
        Self {
            point_map: LazyInit::new(),
            raw_pipe: SpinMutex::new(TracePipeRaw::new(TRACE_RAW_PIPE_CAPACITY)),
            pipe_event: PollSet::new(),
            pipe_notify: IrqNotify::new(),
            sched_notify: IrqNotify::new(),
            cmdline_cache: LazyInit::new(),
            ext_tracepoints: LazyInit::new(),
        }
    }
}

static TRACE_STATE: TraceState = TraceState::new();
static TRACE_PIPE_NOTIFY_WORKER: AtomicBool = AtomicBool::new(false);
static SCHED_TRACE_WORKER_ID: AtomicU64 = AtomicU64::new(0);
static TRACE_PIPE_NOTIFY_WORKER_ID: AtomicU64 = AtomicU64::new(0);

pub struct KernelTraceAux;

impl KernelTraceOps for KernelTraceAux {
    fn current_pid() -> u32 {
        if let Some(pid) = sched::replay_current_pid() {
            return pid;
        }
        try_current_user_irq_view().map_or(0, |task| task.tid())
    }

    fn trace_pipe_push_raw_record(buf: &[u8]) {
        // log::debug!("trace_pipe_push_raw_record: {}", record.len());
        TRACE_STATE.raw_pipe.lock().push_record(
            monotonic_time_nanos(),
            this_cpu_id() as _,
            buf.to_vec(),
        );
        TRACE_STATE.pipe_notify.notify_irq();
    }

    fn trace_cmdline_push(pid: u32) {
        if let Some((comm, len)) = sched::replay_comm(pid) {
            let pname = core::str::from_utf8(&comm[..len]).unwrap_or("unknown");
            TRACE_STATE.cmdline_cache.lock().insert(pid, pname);
            return;
        }
        let Some(curr) = try_current_user_irq_view() else {
            return;
        };
        let mut comm = [0; 16];
        let Some(len) = curr.copy_comm(&mut comm) else {
            return;
        };
        let pname = core::str::from_utf8(&comm[..len]).unwrap_or("unknown");
        TRACE_STATE.cmdline_cache.lock().insert(pid, pname);
    }

    fn write_kernel_text(addr: *mut core::ffi::c_void, data: &[u8]) {
        crate::mm::write_kernel_text(VirtAddr::from_mut_ptr_of(addr), data)
            .expect("Failed to write kernel text");
    }

    fn read_tracepoint_state<R>(id: u32, f: impl FnOnce(&ExtTracePoint<Self>) -> R) -> R {
        let ext_tp = TRACE_STATE
            .ext_tracepoints
            .deref()
            .get(&id)
            .expect("Tracepoint not found");
        f(ext_tp.lock().deref())
    }

    fn write_tracepoint_state<R>(id: u32, f: impl FnOnce(&mut ExtTracePoint<Self>) -> R) -> R {
        let ext_tp = TRACE_STATE
            .ext_tracepoints
            .deref()
            .get(&id)
            .expect("Tracepoint not found");
        let mut ext_tp = ext_tp.lock();
        f(&mut ext_tp)
    }
}

fn start_trace_pipe_notify_worker() -> ax_runtime::task::ThreadHandle {
    if TRACE_PIPE_NOTIFY_WORKER.swap(true, Ordering::AcqRel) {
        panic!("trace pipe notify worker started twice");
    }
    crate::task::spawn_kernel_thread(
        || {
            loop {
                TRACE_STATE.pipe_notify.wait();
                // Trace records are queued before the deferred poll wake.
                unsafe { TRACE_STATE.pipe_event.wake(IoEvents::IN) };
            }
        },
        "trace-pipe-notify".into(),
    )
}

fn publish_trace_worker_id(slot: &AtomicU64, worker: &ax_runtime::task::ThreadHandle, name: &str) {
    let worker_id = worker.id().as_u64();
    assert_ne!(worker_id, 0, "{name} has an invalid scheduler identity");
    slot.compare_exchange(0, worker_id, Ordering::Release, Ordering::Relaxed)
        .unwrap_or_else(|_| panic!("{name} started twice"));
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
    trace_buf: &mut dyn TracePipeOps,
    drain: &mut TextDrain,
    buf: &mut [u8],
) -> usize {
    let mut copy_len = drain.drain_pending(buf);
    if copy_len == buf.len() {
        return copy_len;
    }

    let trace_cmdline_cache = TRACE_STATE.cmdline_cache.lock();
    loop {
        if let Some(record) = trace_buf.peek() {
            let record_str = TraceEntryParser::parse::<KernelTraceAux>(
                &TRACE_STATE.point_map,
                &trace_cmdline_cache,
                record,
            );
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
pub fn tracepoint_init() -> AxResult<()> {
    let (tp_map, ext_tps) =
        global_init_events::<KernelTraceAux>().map_err(|_| AxError::InvalidInput)?;

    let ext_tps = ext_tps
        .into_iter()
        .map(|ext_tp| (ext_tp.id(), Arc::new(SpinNoPreempt::new(ext_tp))))
        .collect::<BTreeMap<_, _>>();

    ax_println!("Initialized {} tracepoints", tp_map.len());
    TRACE_STATE.point_map.init_once(tp_map);
    TRACE_STATE.ext_tracepoints.init_once(ext_tps);
    TRACE_STATE
        .cmdline_cache
        .init_once(SpinMutex::new(TraceCmdLineCache::new(
            NonZero::new(TRACE_CMDLINE_CACHE_SIZE).unwrap(),
        )));
    let sched_worker = sched::start_worker();
    let pipe_worker = start_trace_pipe_notify_worker();
    publish_trace_worker_id(
        &SCHED_TRACE_WORKER_ID,
        &sched_worker,
        "scheduler trace worker",
    );
    publish_trace_worker_id(
        &TRACE_PIPE_NOTIFY_WORKER_ID,
        &pipe_worker,
        "trace pipe notify worker",
    );
    // The hook becomes visible only after both infrastructure identities are
    // published, so their first schedule-in cannot enter the deferred ring.
    sched::install();
    Ok(())
}

/// Initialize events directory in debugfs
fn init_events(fs: Arc<SimpleFs>) -> DirMaker {
    let mut events_root = DirMapping::new();
    let mut subsystem = BTreeMap::new();

    for ext_tp in TRACE_STATE.ext_tracepoints.deref().values() {
        let tp = ext_tp.lock().trace_point();
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
