//! `sched:*` tracepoints.
//!
//! `sched_switch` is fired by the runtime's allocation-free scheduler trace
//! hook after `ax-task` commits a context-switch decision.
//!
//! The other two `sched:*` events are defined next to their emission sites
//! rather than here: `sched_process_fork` in `crate::syscall::task::clone`
//! and `sched_process_exit` in `crate::task::ops`. Registration is by link
//! section, so their physical location does not affect discovery.

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_runtime::task::SchedSwitchRecord;

use super::sched_filter::should_defer_sched_switch;
use crate::task::try_current_user_irq_view;

const DEFERRED_RING_CAPACITY: usize = 256;
const DEFERRED_DRAIN_BATCH: usize = 64;
const TASK_COMM_LEN: usize = 16;

#[derive(Clone, Copy)]
struct DeferredSchedSwitch {
    record: SchedSwitchRecord,
    pid: u32,
    comm_len: u8,
    comm: [u8; TASK_COMM_LEN],
}

impl DeferredSchedSwitch {
    fn capture(record: SchedSwitchRecord) -> Self {
        let mut comm = [0; TASK_COMM_LEN];
        let (pid, comm_len) = try_current_user_irq_view().map_or((0, 0), |task| {
            let len = task.copy_comm(&mut comm).unwrap_or(0);
            (task.tid(), len as u8)
        });
        Self {
            record,
            pid,
            comm_len,
            comm,
        }
    }
}

struct DeferredSchedRing {
    head: AtomicUsize,
    tail: AtomicUsize,
    slots: UnsafeCell<[MaybeUninit<DeferredSchedSwitch>; DEFERRED_RING_CAPACITY]>,
}

impl DeferredSchedRing {
    const fn new() -> Self {
        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            slots: UnsafeCell::new([MaybeUninit::uninit(); DEFERRED_RING_CAPACITY]),
        }
    }

    fn push(&self, record: DeferredSchedSwitch) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= DEFERRED_RING_CAPACITY {
            return false;
        }
        unsafe {
            // Each logical CPU is the sole producer for its ring. The consumer
            // cannot read this slot until the Release head publication below.
            (*self.slots.get())[head % DEFERRED_RING_CAPACITY].write(record);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    fn pop(&self) -> Option<DeferredSchedSwitch> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        let record = unsafe {
            // Acquire observation of head makes the producer's initialized
            // slot visible. This service thread is the sole consumer.
            (*self.slots.get())[tail % DEFERRED_RING_CAPACITY].assume_init_read()
        };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(record)
    }

    fn has_pending(&self) -> bool {
        self.tail.load(Ordering::Relaxed) != self.head.load(Ordering::Acquire)
    }
}

// SAFETY: one IRQ-disabled scheduler producer owns each indexed ring and the
// single deferred worker owns all tail updates. Release/Acquire publication
// excludes overlapping access to every slot.
unsafe impl Sync for DeferredSchedRing {}

static DEFERRED_RINGS: LazyInit<Box<[DeferredSchedRing]>> = LazyInit::new();
static DEFERRED_DROPPED: AtomicU64 = AtomicU64::new(0);
static DRAIN_CPU_CURSOR: AtomicUsize = AtomicUsize::new(0);

struct ReplayIdentity {
    active: AtomicBool,
    owner: AtomicU64,
    pid: AtomicU32,
    comm_len: AtomicU8,
    comm: [AtomicU8; TASK_COMM_LEN],
}

impl ReplayIdentity {
    const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            owner: AtomicU64::new(0),
            pid: AtomicU32::new(0),
            comm_len: AtomicU8::new(0),
            comm: [const { AtomicU8::new(0) }; TASK_COMM_LEN],
        }
    }

    fn belongs_to_current(&self) -> bool {
        if !self.active.load(Ordering::Acquire) || ax_runtime::hal::irq::in_irq_context() {
            return false;
        }
        ax_runtime::task::current_thread_id()
            .is_ok_and(|thread| thread.as_u64() == self.owner.load(Ordering::Relaxed))
    }
}

static REPLAY_IDENTITY: ReplayIdentity = ReplayIdentity::new();

struct ReplayGuard;

impl ReplayGuard {
    fn begin(record: &DeferredSchedSwitch) -> Option<Self> {
        let owner = ax_runtime::task::current_thread_id().ok()?.as_u64();
        for (slot, byte) in REPLAY_IDENTITY.comm.iter().zip(record.comm) {
            slot.store(byte, Ordering::Relaxed);
        }
        REPLAY_IDENTITY
            .comm_len
            .store(record.comm_len, Ordering::Relaxed);
        REPLAY_IDENTITY.pid.store(record.pid, Ordering::Relaxed);
        REPLAY_IDENTITY.owner.store(owner, Ordering::Relaxed);
        REPLAY_IDENTITY.active.store(true, Ordering::Release);
        Some(Self)
    }
}

impl Drop for ReplayGuard {
    fn drop(&mut self) {
        REPLAY_IDENTITY.active.store(false, Ordering::Release);
    }
}

pub(super) fn replay_current_pid() -> Option<u32> {
    REPLAY_IDENTITY
        .belongs_to_current()
        .then(|| REPLAY_IDENTITY.pid.load(Ordering::Relaxed))
}

pub(super) fn replay_comm(pid: u32) -> Option<([u8; TASK_COMM_LEN], usize)> {
    if !REPLAY_IDENTITY.belongs_to_current() || REPLAY_IDENTITY.pid.load(Ordering::Relaxed) != pid {
        return None;
    }
    let len = usize::from(REPLAY_IDENTITY.comm_len.load(Ordering::Relaxed));
    if len == 0 || len > TASK_COMM_LEN {
        return None;
    }
    let mut comm = [0; TASK_COMM_LEN];
    for (byte, slot) in comm.iter_mut().zip(&REPLAY_IDENTITY.comm) {
        *byte = slot.load(Ordering::Relaxed);
    }
    Some((comm, len))
}

ktracepoint::define_event_trace!(
    sched_switch,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(sched),
    TP_PROTO(prev_tid: u64, next_tid: u64, prev_state: u32),
    TP_STRUCT__entry {
        prev_tid: u64,
        next_tid: u64,
        prev_state: u32,
    },
    TP_fast_assign {
        prev_tid: prev_tid,
        next_tid: next_tid,
        prev_state: prev_state,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "prev_tid={} next_tid={} prev_state={}",
            __entry.prev_tid,
            __entry.next_tid,
            __entry.prev_state,
        )
    })
);

pub(super) fn install() {
    let rings = (0..ax_runtime::hal::cpu_num())
        .map(|_| DeferredSchedRing::new())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    DEFERRED_RINGS.init_once(rings);
    ax_runtime::task::install_sched_switch_trace_hook(on_sched_switch);
}

pub(super) fn start_worker() -> ax_runtime::task::ThreadHandle {
    crate::task::spawn_kernel_thread(
        || {
            loop {
                super::TRACE_STATE.sched_notify.wait();
                while drain_deferred(DEFERRED_DRAIN_BATCH, replay_sched_switch) {
                    ax_runtime::task::yield_current_cpu().unwrap_or_else(|error| {
                        panic!("scheduler trace worker failed to yield: {error}")
                    });
                }
            }
        },
        "sched-switch-trace".into(),
    )
}

fn on_sched_switch(record: SchedSwitchRecord) {
    if !__sched_switch.key_is_enabled() {
        return;
    }
    let worker_ids = [
        super::SCHED_TRACE_WORKER_ID.load(Ordering::Acquire),
        super::TRACE_PIPE_NOTIFY_WORKER_ID.load(Ordering::Acquire),
    ];
    if !should_defer_sched_switch(true, worker_ids, record.previous_thread, record.next_thread) {
        return;
    }
    if publish_deferred(DeferredSchedSwitch::capture(record)) {
        super::TRACE_STATE.sched_notify.notify_irq();
    }
}

fn publish_deferred(record: DeferredSchedSwitch) -> bool {
    let Some(rings) = DEFERRED_RINGS.get() else {
        DEFERRED_DROPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    let Some(ring) = rings.get(record.record.cpu.as_u32() as usize) else {
        DEFERRED_DROPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    if !ring.push(record) {
        DEFERRED_DROPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    true
}

fn drain_deferred(limit: usize, mut consume: impl FnMut(DeferredSchedSwitch)) -> bool {
    let Some(rings) = DEFERRED_RINGS.get() else {
        return false;
    };
    if rings.is_empty() || limit == 0 {
        return rings.iter().any(DeferredSchedRing::has_pending);
    }
    let mut start = DRAIN_CPU_CURSOR.load(Ordering::Relaxed) % rings.len();
    let mut drained = 0;
    while drained < limit {
        let mut progressed = false;
        for offset in 0..rings.len() {
            let cpu = (start + offset) % rings.len();
            if let Some(record) = rings[cpu].pop() {
                consume(record);
                drained += 1;
                progressed = true;
                start = (cpu + 1) % rings.len();
                if drained == limit {
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    DRAIN_CPU_CURSOR.store(start, Ordering::Relaxed);
    rings.iter().any(DeferredSchedRing::has_pending)
}

fn replay_sched_switch(record: DeferredSchedSwitch) {
    let Some(_replay) = ReplayGuard::begin(&record) else {
        DEFERRED_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    trace_sched_switch(
        record.record.previous_thread,
        record.record.next_thread,
        record.record.reason,
    );
}
