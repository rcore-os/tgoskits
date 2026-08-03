//! Fixed-capacity guest architectural timer IRQ entry trace.
//!
//! Recording is allocation-free and lock-free. Formatting and filesystem I/O
//! are deliberately left to consumers after [`stop_and_snapshot`] quiesces all
//! in-flight writers.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

const TRACE_CAPACITY: usize = 262_144;

/// One guest timer IRQ observation in the guest virtual-counter domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerIrqRecord {
    /// Global reservation order within this capture.
    pub sequence: u64,
    /// Guest logical CPU, equal to the AxVisor vCPU index for the RT profile.
    pub vcpu_id: u32,
    /// Guest-visible architectural timer interrupt number.
    pub irq: u32,
    /// `CNTVCT_EL0`-domain counter at handler entry.
    pub entry_ticks: u64,
    /// Counter ticks spent in the StarryOS/ArceOS timer handler.
    pub handler_ticks: u64,
}

struct TraceSlot {
    committed: AtomicUsize,
    record: UnsafeCell<MaybeUninit<TimerIrqRecord>>,
}

impl TraceSlot {
    const fn new() -> Self {
        Self {
            committed: AtomicUsize::new(0),
            record: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: a slot has one writer selected by `next.fetch_add`. Readers access
// the record only after the writer publishes the matching committed token with
// Release ordering, and capture reset waits for all writers to quiesce.
unsafe impl Sync for TraceSlot {}

struct TraceBuffer<const N: usize> {
    enabled: AtomicBool,
    next: AtomicUsize,
    active_writers: AtomicUsize,
    dropped: AtomicUsize,
    incomplete: AtomicUsize,
    start_ticks: AtomicU64,
    end_ticks: AtomicU64,
    slots: [TraceSlot; N],
}

impl<const N: usize> TraceBuffer<N> {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            active_writers: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            incomplete: AtomicUsize::new(0),
            start_ticks: AtomicU64::new(0),
            end_ticks: AtomicU64::new(0),
            slots: [const { TraceSlot::new() }; N],
        }
    }

    fn start(&self, now_ticks: u64) {
        self.stop_writers();
        let previous_len = self.next.load(Ordering::Relaxed).min(N);
        for slot in &self.slots[..previous_len] {
            slot.committed.store(0, Ordering::Relaxed);
        }
        self.next.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        self.incomplete.store(0, Ordering::Relaxed);
        self.start_ticks.store(now_ticks, Ordering::Relaxed);
        self.end_ticks.store(0, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
    }

    fn stop(&self, now_ticks: u64) {
        self.stop_writers();
        self.end_ticks.store(now_ticks, Ordering::Release);
    }

    fn stop_writers(&self) {
        self.enabled.store(false, Ordering::Release);
        while self.active_writers.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
    }

    fn reserve(&self) -> Option<Reservation<'_, N>> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        self.active_writers.fetch_add(1, Ordering::AcqRel);
        if !self.enabled.load(Ordering::Acquire) {
            self.active_writers.fetch_sub(1, Ordering::Release);
            return None;
        }
        let index = self.next.fetch_add(1, Ordering::Relaxed);
        if index >= N {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            self.active_writers.fetch_sub(1, Ordering::Release);
            return None;
        }
        Some(Reservation {
            buffer: self,
            index,
            finished: false,
        })
    }

    fn record(&self, index: usize) -> Option<TimerIrqRecord> {
        let slot = self.slots.get(index)?;
        if slot.committed.load(Ordering::Acquire) != index + 1 {
            return None;
        }
        // SAFETY: the matching committed token was acquired above. The record
        // is Copy, the capture is stopped before public snapshots are read, and
        // this slot is not reused until the next reset.
        Some(unsafe { *(*slot.record.get()).assume_init_ref() })
    }
}

struct Reservation<'a, const N: usize> {
    buffer: &'a TraceBuffer<N>,
    index: usize,
    finished: bool,
}

impl<const N: usize> Reservation<'_, N> {
    fn finish(mut self, record: TimerIrqRecord) {
        let slot = &self.buffer.slots[self.index];
        // SAFETY: `index` was returned exactly once by `fetch_add`, so this
        // reservation is the slot's only writer until it publishes commit.
        unsafe { (*slot.record.get()).write(record) };
        slot.committed.store(self.index + 1, Ordering::Release);
        self.finished = true;
        self.buffer.active_writers.fetch_sub(1, Ordering::Release);
    }
}

impl<const N: usize> Drop for Reservation<'_, N> {
    fn drop(&mut self) {
        if !self.finished {
            self.buffer.incomplete.fetch_add(1, Ordering::Relaxed);
            self.buffer.active_writers.fetch_sub(1, Ordering::Release);
        }
    }
}

static TIMER_IRQ_TRACE: TraceBuffer<TRACE_CAPACITY> = TraceBuffer::new();

/// A reserved trace slot spanning one timer IRQ handler invocation.
pub struct PendingTimerIrq {
    reservation: Reservation<'static, TRACE_CAPACITY>,
    vcpu_id: u32,
    irq: u32,
    entry_ticks: u64,
}

impl PendingTimerIrq {
    /// Completes and publishes the IRQ record.
    pub fn finish(self, finished_ticks: u64) {
        let sequence = self.reservation.index as u64;
        let record = TimerIrqRecord {
            sequence,
            vcpu_id: self.vcpu_id,
            irq: self.irq,
            entry_ticks: self.entry_ticks,
            handler_ticks: finished_ticks.saturating_sub(self.entry_ticks),
        };
        self.reservation.finish(record);
    }
}

/// Starts a fresh guest IRQ capture.
pub fn start() {
    TIMER_IRQ_TRACE.start(ax_hal::time::current_ticks());
}

/// Reserves an IRQ trace record without allocating or taking a lock.
pub fn begin_timer_irq(vcpu_id: usize, irq: u32, entry_ticks: u64) -> Option<PendingTimerIrq> {
    let vcpu_id = u32::try_from(vcpu_id).ok()?;
    Some(PendingTimerIrq {
        reservation: TIMER_IRQ_TRACE.reserve()?,
        vcpu_id,
        irq,
        entry_ticks,
    })
}

/// Immutable metadata and indexed access to one stopped trace capture.
#[derive(Clone, Copy, Debug)]
pub struct TimerIrqTraceSnapshot {
    /// Generic counter frequency shared by all guest vCPUs.
    pub counter_frequency_hz: u64,
    /// Counter at capture activation.
    pub start_ticks: u64,
    /// Counter after all writers were stopped.
    pub end_ticks: u64,
    /// Number of in-capacity reservations, including any incomplete record.
    pub record_count: usize,
    /// Events rejected after the fixed buffer filled.
    pub dropped: usize,
    /// Reservations dropped without publication.
    pub incomplete: usize,
}

impl TimerIrqTraceSnapshot {
    /// Returns one committed record by reservation index.
    pub fn record(&self, index: usize) -> Option<TimerIrqRecord> {
        if index >= self.record_count {
            return None;
        }
        TIMER_IRQ_TRACE.record(index)
    }
}

/// Stops recording, waits for in-flight IRQ writers, and returns a snapshot.
pub fn stop_and_snapshot() -> TimerIrqTraceSnapshot {
    let end_ticks = ax_hal::time::current_ticks();
    TIMER_IRQ_TRACE.stop(end_ticks);
    TimerIrqTraceSnapshot {
        counter_frequency_hz: counter_frequency_hz(),
        start_ticks: TIMER_IRQ_TRACE.start_ticks.load(Ordering::Acquire),
        end_ticks,
        record_count: TIMER_IRQ_TRACE
            .next
            .load(Ordering::Acquire)
            .min(TRACE_CAPACITY),
        dropped: TIMER_IRQ_TRACE.dropped.load(Ordering::Acquire),
        incomplete: TIMER_IRQ_TRACE.incomplete.load(Ordering::Acquire),
    }
}

#[cfg(target_arch = "aarch64")]
fn counter_frequency_hz() -> u64 {
    let frequency: u64;
    // SAFETY: reading CNTFRQ_EL0 is side-effect free and is available at EL1.
    unsafe { core::arch::asm!("mrs {frequency}, CNTFRQ_EL0", frequency = out(reg) frequency) };
    frequency
}

#[cfg(not(target_arch = "aarch64"))]
fn counter_frequency_hz() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    use super::{TimerIrqRecord, TraceBuffer};

    fn record(sequence: u64) -> TimerIrqRecord {
        TimerIrqRecord {
            sequence,
            vcpu_id: 0,
            irq: 27,
            entry_ticks: 100 + sequence,
            handler_ticks: 3,
        }
    }

    #[test]
    fn fixed_buffer_preserves_records_and_counts_overflow() {
        let buffer = TraceBuffer::<2>::new();
        buffer.start(10);
        buffer.reserve().unwrap().finish(record(0));
        buffer.reserve().unwrap().finish(record(1));
        assert!(buffer.reserve().is_none());
        buffer.stop(20);

        assert_eq!(buffer.record(0), Some(record(0)));
        assert_eq!(buffer.record(1), Some(record(1)));
        assert_eq!(buffer.dropped.load(Ordering::Relaxed), 1);
    }
}
