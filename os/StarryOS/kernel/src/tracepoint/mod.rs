//! See Linux Documentation for details: <https://docs.kernel.org/trace/ftrace.html>
mod control;
mod registry;
mod sched;
mod sched_filter;
mod trace;
mod trace_pipe;

use alloc::{collections::BTreeMap, string::ToString, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    num::NonZero,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_lazyinit::LazyInit;
use ax_runtime::hal::{percpu::this_cpu_id, time::monotonic_time_nanos};
use ax_sync::PiMutex;
use axfs_ng_vfs::NodePermission;
use axpoll::{IoEvents, PollSet};
use ktracepoint::*;
pub use registry::KernelExtTracePoint;
use registry::TracepointReclaimer;

use crate::{
    pseudofs::{DirMaker, DirMapping, SeqObject, SimpleDir, SimpleFs, SpecialFsFile},
    task::{future::IrqNotify, try_current_user_irq_view},
};

/// Maximum number of trace records kept in the raw trace pipe ring buffer.
const TRACE_RAW_PIPE_CAPACITY: usize = 4096;
/// Maximum number of trace ingress records awaiting task-context processing.
const TRACE_INGRESS_CAPACITY: usize = 1024;
/// Maximum raw event payload copied from a tracepoint fire path.
const TRACE_RAW_RECORD_BYTES: usize = 256;
/// Maximum number of ingress records processed without yielding.
const TRACE_INGRESS_DRAIN_BATCH: usize = 128;
/// Linux task command names are bounded by `TASK_COMM_LEN`.
const TRACE_TASK_COMM_LEN: usize = 16;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TraceIngressKind {
    Raw,
    Cmdline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TraceIngressRecord {
    kind: TraceIngressKind,
    epoch: u64,
    timestamp: u64,
    id: u32,
    len: u16,
    bytes: [u8; TRACE_RAW_RECORD_BYTES],
}

impl TraceIngressRecord {
    fn raw(epoch: u64, timestamp: u64, cpu_id: u32, event: &[u8]) -> Option<Self> {
        if event.len() > TRACE_RAW_RECORD_BYTES {
            return None;
        }
        let mut bytes = [0; TRACE_RAW_RECORD_BYTES];
        bytes[..event.len()].copy_from_slice(event);
        Some(Self {
            kind: TraceIngressKind::Raw,
            epoch,
            timestamp,
            id: cpu_id,
            len: event.len() as u16,
            bytes,
        })
    }

    fn cmdline(pid: u32, comm: &[u8]) -> Self {
        let len = comm.len().min(TRACE_TASK_COMM_LEN);
        let mut bytes = [0; TRACE_RAW_RECORD_BYTES];
        bytes[..len].copy_from_slice(&comm[..len]);
        Self {
            kind: TraceIngressKind::Cmdline,
            epoch: 0,
            timestamp: 0,
            id: pid,
            len: len as u16,
            bytes,
        }
    }
}

struct TraceIngressSlot {
    ready: AtomicBool,
    record: UnsafeCell<MaybeUninit<TraceIngressRecord>>,
}

impl TraceIngressSlot {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            record: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: a successful enqueue reservation gives one producer exclusive
// access to a slot. The single consumer reads it only after Release
// publication and returns it to producers before advancing `dequeue`.
unsafe impl Sync for TraceIngressSlot {}

/// Fixed-capacity MPSC ingress with one task-context consumer.
struct TraceIngressRing<const CAPACITY: usize> {
    slots: [TraceIngressSlot; CAPACITY],
    enqueue: AtomicUsize,
    dequeue: AtomicUsize,
}

impl<const CAPACITY: usize> TraceIngressRing<CAPACITY> {
    const fn new() -> Self {
        assert!(CAPACITY != 0);
        Self {
            slots: [const { TraceIngressSlot::new() }; CAPACITY],
            enqueue: AtomicUsize::new(0),
            dequeue: AtomicUsize::new(0),
        }
    }

    fn push(&self, record: TraceIngressRecord) -> bool {
        let mut tail = self.enqueue.load(Ordering::Relaxed);
        loop {
            let head = self.dequeue.load(Ordering::Acquire);
            if tail.wrapping_sub(head) >= CAPACITY {
                return false;
            }
            match self.enqueue.compare_exchange_weak(
                tail,
                tail.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let slot = &self.slots[tail % CAPACITY];
                    debug_assert!(!slot.ready.load(Ordering::Acquire));
                    // SAFETY: this producer exclusively owns the reserved
                    // logical position until it publishes `ready`.
                    unsafe { (*slot.record.get()).write(record) };
                    slot.ready.store(true, Ordering::Release);
                    return true;
                }
                Err(observed) => tail = observed,
            }
        }
    }

    fn pop(&self) -> Option<TraceIngressRecord> {
        let head = self.dequeue.load(Ordering::Relaxed);
        let slot = &self.slots[head % CAPACITY];
        if !slot.ready.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: Acquire observation of `ready` proves that the producer
        // initialized this slot, and only this consumer advances `dequeue`.
        let record = unsafe { (*slot.record.get()).assume_init_read() };
        slot.ready.store(false, Ordering::Release);
        self.dequeue.store(head.wrapping_add(1), Ordering::Release);
        Some(record)
    }

    fn has_pending(&self) -> bool {
        let head = self.dequeue.load(Ordering::Relaxed);
        self.slots[head % CAPACITY].ready.load(Ordering::Acquire)
    }
}

struct TraceState {
    point_map: LazyInit<TracePointMap<KernelTraceAux>>,
    raw_pipe: PiMutex<TracePipeRaw>,
    raw_epoch: AtomicU64,
    ingress: TraceIngressRing<TRACE_INGRESS_CAPACITY>,
    pipe_event: PollSet,
    pipe_notify: IrqNotify,
    sched_notify: IrqNotify,
    reclaimer: TracepointReclaimer,
    cmdline_cache: LazyInit<PiMutex<TraceCmdLineCache>>,
    ext_tracepoints: LazyInit<BTreeMap<u32, KernelExtTracePoint>>,
}

impl TraceState {
    const fn new() -> Self {
        Self {
            point_map: LazyInit::new(),
            raw_pipe: PiMutex::new(TracePipeRaw::new(TRACE_RAW_PIPE_CAPACITY)),
            raw_epoch: AtomicU64::new(0),
            ingress: TraceIngressRing::new(),
            pipe_event: PollSet::new(),
            pipe_notify: IrqNotify::new(),
            sched_notify: IrqNotify::new(),
            reclaimer: TracepointReclaimer::new(),
            cmdline_cache: LazyInit::new(),
            ext_tracepoints: LazyInit::new(),
        }
    }
}

static TRACE_STATE: TraceState = TraceState::new();
static TRACE_PIPE_NOTIFY_WORKER: AtomicBool = AtomicBool::new(false);
static TRACE_INGRESS_DROPPED: AtomicU64 = AtomicU64::new(0);
static SCHED_TRACE_WORKER_ID: AtomicU64 = AtomicU64::new(0);
static TRACE_PIPE_NOTIFY_WORKER_ID: AtomicU64 = AtomicU64::new(0);
static TRACEPOINT_RECLAIM_WORKER_ID: AtomicU64 = AtomicU64::new(0);

pub struct KernelTraceAux;

impl KernelTraceOps for KernelTraceAux {
    fn current_pid() -> u32 {
        if let Some(pid) = sched::replay_current_pid() {
            return pid;
        }
        try_current_user_irq_view().map_or(0, |task| task.tid())
    }

    fn trace_pipe_push_raw_record(buf: &[u8]) {
        let epoch = TRACE_STATE.raw_epoch.load(Ordering::Acquire);
        let Some(record) =
            TraceIngressRecord::raw(epoch, monotonic_time_nanos(), this_cpu_id() as _, buf)
        else {
            TRACE_INGRESS_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        };
        publish_trace_ingress(record);
    }

    fn trace_cmdline_push(pid: u32) {
        if let Some((comm, len)) = sched::replay_comm(pid) {
            publish_trace_ingress(TraceIngressRecord::cmdline(pid, &comm[..len]));
            return;
        }
        let Some(curr) = try_current_user_irq_view() else {
            return;
        };
        let mut comm = [0; 16];
        let Some(len) = curr.copy_comm(&mut comm) else {
            return;
        };
        publish_trace_ingress(TraceIngressRecord::cmdline(pid, &comm[..len]));
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

#[cfg(axtest)]
pub(crate) fn callbacks_run_without_raw_guard_for_test() -> bool {
    let tracepoint =
        KernelExtTracePoint::new(sched::tracepoint_state_for_test(), &TRACE_STATE.reclaimer);
    let read_result = tracepoint.read(|_| ax_runtime::task::yield_current_cpu());
    let write_result = tracepoint.update(|_| ax_runtime::task::yield_current_cpu());
    let blocked_retirement = tracepoint.read(|_| {
        // Cross both reader-counter epochs while the first epoch is still
        // leased. Neither generation sharing that counter may be reclaimed.
        for _ in 0..3 {
            tracepoint.update(|_| {});
        }
        TRACE_STATE.reclaimer.drain_for_test()
    });
    read_result.is_ok()
        && write_result.is_ok()
        && blocked_retirement
        && !TRACE_STATE.reclaimer.drain_for_test()
}

fn publish_trace_ingress(record: TraceIngressRecord) {
    if !TRACE_STATE.ingress.push(record) {
        TRACE_INGRESS_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // The complete fixed-size record is Release-published before the sticky
    // worker notification.
    TRACE_STATE.pipe_notify.notify_irq();
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TraceIngressDrain {
    pending: bool,
    raw_published: bool,
}

fn drain_trace_ingress(limit: usize) -> TraceIngressDrain {
    let mut drained = 0;
    let mut raw_published = false;
    while drained < limit {
        let Some(record) = TRACE_STATE.ingress.pop() else {
            break;
        };
        drained += 1;
        match record.kind {
            TraceIngressKind::Raw => {
                if record.epoch != TRACE_STATE.raw_epoch.load(Ordering::Acquire) {
                    continue;
                }
                let event = record.bytes[..usize::from(record.len)].to_vec();
                TRACE_STATE
                    .raw_pipe
                    .lock()
                    .push_record(record.timestamp, record.id, event);
                raw_published = true;
            }
            TraceIngressKind::Cmdline => {
                let comm = core::str::from_utf8(&record.bytes[..usize::from(record.len)])
                    .unwrap_or("unknown");
                TRACE_STATE.cmdline_cache.lock().insert(record.id, comm);
            }
        }
    }
    TraceIngressDrain {
        pending: TRACE_STATE.ingress.has_pending(),
        raw_published,
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
                loop {
                    let drain = drain_trace_ingress(TRACE_INGRESS_DRAIN_BATCH);
                    if drain.raw_published {
                        // Trace records are queued before the deferred poll wake.
                        unsafe { TRACE_STATE.pipe_event.wake(IoEvents::IN) };
                    }
                    if !drain.pending {
                        break;
                    }
                    ax_runtime::task::yield_current_cpu().unwrap_or_else(|error| {
                        panic!("trace ingress worker failed to yield: {error}")
                    });
                }
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
        .map(|ext_tp| {
            (
                ext_tp.id(),
                KernelExtTracePoint::new(ext_tp, &TRACE_STATE.reclaimer),
            )
        })
        .collect::<BTreeMap<_, _>>();

    ax_println!("Initialized {} tracepoints", tp_map.len());
    TRACE_STATE.point_map.init_once(tp_map);
    TRACE_STATE.ext_tracepoints.init_once(ext_tps);
    TRACE_STATE
        .cmdline_cache
        .init_once(PiMutex::new(TraceCmdLineCache::new(
            NonZero::new(TRACE_CMDLINE_CACHE_SIZE).unwrap(),
        )));
    let sched_worker = sched::start_worker();
    let pipe_worker = start_trace_pipe_notify_worker();
    let reclaim_worker = TRACE_STATE.reclaimer.start_worker();
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
    publish_trace_worker_id(
        &TRACEPOINT_RECLAIM_WORKER_ID,
        &reclaim_worker,
        "tracepoint reclaim worker",
    );
    // The hook becomes visible only after every infrastructure identity is
    // published, so their first schedule-in cannot enter the deferred ring.
    sched::install();
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

#[cfg(test)]
mod tests {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    use super::*;

    #[global_allocator]
    static ALLOCATOR: AuditAllocator = AuditAllocator;

    std::thread_local! {
        static AUDIT_ENABLED: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    struct AuditAllocator;

    // SAFETY: every operation delegates to the system allocator with the
    // original pointer and layout. Thread-local counters are observational.
    unsafe impl GlobalAlloc for AuditAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                count_allocation();
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) };
        }
    }

    #[test]
    fn trace_ingress_is_bounded_fifo_and_preserves_payload() {
        let ring = TraceIngressRing::<2>::new();
        let first = TraceIngressRecord::raw(1, 10, 0, b"first").unwrap();
        let second = TraceIngressRecord::raw(1, 20, 1, b"second").unwrap();
        let overflow = TraceIngressRecord::raw(1, 30, 2, b"overflow").unwrap();

        assert!(ring.push(first));
        assert!(ring.push(second));
        assert!(!ring.push(overflow));
        assert_eq!(ring.pop(), Some(first));
        assert_eq!(ring.pop(), Some(second));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn trace_ingress_rejects_oversized_records_without_allocation() {
        let ring = TraceIngressRing::<1>::new();
        let oversized = [0_u8; TRACE_RAW_RECORD_BYTES + 1];

        let allocations = audit_allocations(|| {
            assert!(TraceIngressRecord::raw(1, 10, 0, &oversized).is_none());
            assert!(ring.push(TraceIngressRecord::cmdline(7, b"worker")));
        });

        assert_eq!(allocations, 0);
    }

    fn audit_allocations(operation: impl FnOnce()) -> usize {
        AUDIT_ENABLED.with(|enabled| {
            assert!(!enabled.replace(true));
            ALLOCATIONS.with(|allocations| allocations.set(0));
            operation();
            enabled.set(false);
            ALLOCATIONS.with(Cell::get)
        })
    }

    fn count_allocation() {
        let enabled = AUDIT_ENABLED.try_with(Cell::get).unwrap_or(false);
        if enabled {
            let _ = ALLOCATIONS.try_with(|allocations| {
                allocations.set(allocations.get() + 1);
            });
        }
    }
}
