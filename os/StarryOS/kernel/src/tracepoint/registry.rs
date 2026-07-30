use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::{
    mem::replace,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_kspin::SpinNoPreempt;
use ax_sync::PiMutex;
use ktracepoint::{ExtTracePoint, TracePoint};

use super::KernelTraceAux;
use crate::task::future::IrqNotify;

/// Maximum retired callback snapshots reclaimed without yielding.
const TRACEPOINT_RECLAIM_BATCH: usize = 64;

struct TracepointSnapshotState {
    current: Arc<ExtTracePoint<KernelTraceAux>>,
    epoch: usize,
}

struct KernelExtTracePointState {
    snapshot: SpinNoPreempt<TracepointSnapshotState>,
    readers: [AtomicUsize; 2],
    update: PiMutex<()>,
    reclaimer: &'static TracepointReclaimer,
}

struct RetiredTracepoint {
    state: Arc<KernelExtTracePointState>,
    snapshot: Arc<ExtTracePoint<KernelTraceAux>>,
    reader_epoch: usize,
}

/// Generation-published tracepoint state.
///
/// Scheduler and IRQ readers hold the raw gate only while acquiring a
/// generation lease. Callback dispatch runs after releasing that gate. Task
/// writers construct a complete replacement under the PI control mutex and
/// swap it through the raw gate. A task worker reclaims retired generations
/// after the corresponding readers leave, so callback dispatch may safely
/// re-enter the management path.
#[derive(Clone)]
pub struct KernelExtTracePoint {
    state: Arc<KernelExtTracePointState>,
}

struct TracepointSnapshotLease<'a> {
    snapshot: Option<Arc<ExtTracePoint<KernelTraceAux>>>,
    readers: &'a AtomicUsize,
    reclaimer: &'static TracepointReclaimer,
}

impl Drop for TracepointSnapshotLease<'_> {
    fn drop(&mut self) {
        // A writer retains the retired generation until this counter reaches
        // zero. Release our Arc first so its final allocation can never be
        // reclaimed from scheduler or hard-IRQ context.
        drop(self.snapshot.take());
        if self.readers.fetch_sub(1, Ordering::Release) == 1 {
            self.reclaimer.notify.notify_irq();
        }
    }
}

impl Deref for TracepointSnapshotLease<'_> {
    type Target = ExtTracePoint<KernelTraceAux>;

    fn deref(&self) -> &Self::Target {
        self.snapshot
            .as_deref()
            .expect("tracepoint snapshot lease was already released")
    }
}

impl KernelExtTracePoint {
    pub(super) fn new(
        tracepoint: ExtTracePoint<KernelTraceAux>,
        reclaimer: &'static TracepointReclaimer,
    ) -> Self {
        Self {
            state: Arc::new(KernelExtTracePointState {
                snapshot: SpinNoPreempt::new(TracepointSnapshotState {
                    current: Arc::new(tracepoint),
                    epoch: 0,
                }),
                readers: [AtomicUsize::new(0), AtomicUsize::new(0)],
                update: PiMutex::new(()),
                reclaimer,
            }),
        }
    }

    fn acquire_snapshot(&self) -> TracepointSnapshotLease<'_> {
        let snapshot = self.state.snapshot.lock();
        let readers = &self.state.readers[snapshot.epoch % self.state.readers.len()];
        readers.fetch_add(1, Ordering::AcqRel);
        let current = Arc::clone(&snapshot.current);
        drop(snapshot);
        TracepointSnapshotLease {
            snapshot: Some(current),
            readers,
            reclaimer: self.state.reclaimer,
        }
    }

    /// Runs a tracepoint read or callback dispatch without retaining the raw
    /// snapshot gate.
    pub fn read<R>(&self, operation: impl FnOnce(&ExtTracePoint<KernelTraceAux>) -> R) -> R {
        let snapshot = self.acquire_snapshot();
        operation(&snapshot)
    }

    /// Applies a task-context update and publishes it as one new generation.
    pub fn update<R>(&self, operation: impl FnOnce(&mut ExtTracePoint<KernelTraceAux>) -> R) -> R {
        ax_runtime::task::validate_blocking_context()
            .expect("tracepoint updates require a preemptible task context");
        let _update = self.state.update.lock();
        let current = {
            let snapshot = self.state.snapshot.lock();
            Arc::clone(&snapshot.current)
        };
        let tracepoint = current.trace_point();
        let was_enabled = current.has_callbacks();
        let mut next = current.as_ref().clone();
        let result = operation(&mut next);
        let is_enabled = next.has_callbacks();
        let next = Arc::new(next);
        let (retired, retired_epoch) = {
            if was_enabled && !is_enabled {
                tracepoint.set_callback_gate(false);
            }
            let mut snapshot = self.state.snapshot.lock();
            let retired_epoch = snapshot.epoch % self.state.readers.len();
            let retired = replace(&mut snapshot.current, next);
            snapshot.epoch = snapshot.epoch.wrapping_add(1);
            (retired, retired_epoch)
        };
        if !was_enabled && is_enabled {
            tracepoint.set_callback_gate(true);
        }
        drop(current);

        if self.state.readers[retired_epoch].load(Ordering::Acquire) == 0 {
            drop(retired);
        } else {
            self.state.reclaimer.enqueue(RetiredTracepoint {
                state: Arc::clone(&self.state),
                snapshot: retired,
                reader_epoch: retired_epoch,
            });
        }
        result
    }

    /// Returns the immutable static tracepoint descriptor.
    pub fn trace_point(&self) -> &'static TracePoint<KernelTraceAux> {
        self.read(ExtTracePoint::trace_point)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TracepointReclaimDrain {
    pending: bool,
    runnable: bool,
}

pub(super) struct TracepointReclaimer {
    queue: PiMutex<VecDeque<RetiredTracepoint>>,
    notify: IrqNotify,
    started: AtomicBool,
}

impl TracepointReclaimer {
    pub(super) const fn new() -> Self {
        Self {
            queue: PiMutex::new(VecDeque::new()),
            notify: IrqNotify::new(),
            started: AtomicBool::new(false),
        }
    }

    fn enqueue(&self, retired: RetiredTracepoint) {
        self.queue.lock().push_back(retired);
        self.notify.notify();
    }

    fn drain(&self, limit: usize) -> TracepointReclaimDrain {
        let retired = {
            let mut queue = self.queue.lock();
            let count = limit.min(queue.len());
            queue.drain(..count).collect::<Vec<_>>()
        };
        let mut blocked = Vec::new();
        for retired in retired {
            if retired.state.readers[retired.reader_epoch].load(Ordering::Acquire) == 0 {
                drop(retired.snapshot);
            } else {
                blocked.push(retired);
            }
        }

        let mut queue = self.queue.lock();
        queue.extend(blocked);
        TracepointReclaimDrain {
            pending: !queue.is_empty(),
            runnable: queue.iter().any(|retired| {
                retired.state.readers[retired.reader_epoch].load(Ordering::Acquire) == 0
            }),
        }
    }

    pub(super) fn start_worker(&'static self) -> ax_runtime::task::ThreadHandle {
        if self.started.swap(true, Ordering::AcqRel) {
            panic!("tracepoint reclaim worker started twice");
        }
        crate::task::spawn_kernel_thread(
            move || loop {
                self.notify.wait();
                loop {
                    let drain = self.drain(TRACEPOINT_RECLAIM_BATCH);
                    if !drain.pending || !drain.runnable {
                        break;
                    }
                    ax_runtime::task::yield_current_cpu().unwrap_or_else(|error| {
                        panic!("tracepoint reclaim worker failed to yield: {error}")
                    });
                }
            },
            "tracepoint-reclaim".into(),
        )
    }

    #[cfg(axtest)]
    pub(super) fn drain_for_test(&self) -> bool {
        self.drain(TRACEPOINT_RECLAIM_BATCH).pending
    }
}
