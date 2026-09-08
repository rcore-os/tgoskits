//! IRQ-backed task input for a HAL-owned console without a runtime UART.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use ax_lazyinit::OnceLock;
use axpoll::IoEvents;
use axpoll_set::PollSet;

use crate::{
    RuntimeError, RuntimeResult,
    serial::{
        RxFlag, RxItem,
        spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
    },
    sync::SpinLock,
    task::{CpuId, CpuSet, FixedIrqWorkerSignal, WaitQueue},
};

const RAW_RX_CAPACITY: usize = 4_096;

static RAW_INPUT: OnceLock<RuntimeResult<Arc<RawInputRuntime>>> = OnceLock::new();

struct RawInputRuntime {
    available: SpinLock<Option<SpscConsumer<RxItem>>>,
    overflow: AtomicBool,
    progress: WaitQueue,
    poll_source: Arc<PollSet>,
    worker_signal: FixedIrqWorkerSignal,
    irq_handle: OnceLock<ax_hal::irq::IrqHandle>,
}

/// Unique task-side consumer for the HAL console's IRQ-backed RX queue.
pub(crate) struct RawConsoleInput {
    consumer: SpinLock<Option<SpscConsumer<RxItem>>>,
    runtime: Arc<RawInputRuntime>,
}

pub(crate) fn take_input() -> RuntimeResult<RawConsoleInput> {
    let runtime = get_or_initialize_result(&RAW_INPUT, init_raw_input)?.clone();
    let consumer = runtime
        .available
        .lock_irqsave()
        .take()
        .ok_or(RuntimeError::SerialConsoleBusy)?;
    Ok(RawConsoleInput {
        consumer: SpinLock::new(Some(consumer)),
        runtime,
    })
}

fn get_or_initialize_result<T, E: Clone>(
    cell: &OnceLock<Result<T, E>>,
    initialize: impl FnOnce() -> Result<T, E>,
) -> Result<&T, E> {
    match cell.call_once(initialize) {
        Ok(value) => Ok(value),
        Err(error) => Err(error.clone()),
    }
}

fn init_raw_input() -> RuntimeResult<Arc<RawInputRuntime>> {
    let irq = ax_hal::console::irq_num().ok_or(RuntimeError::OperationNotSupported)?;
    let owner_cpu = ax_hal::percpu::this_cpu_id();
    let task_cpu =
        u32::try_from(owner_cpu).map_err(|_| RuntimeError::InvalidCpu { cpu: owner_cpu })?;
    let mut affinity = CpuSet::empty(ax_hal::cpu_num());
    if !affinity.insert(CpuId::new(task_cpu)) {
        return Err(RuntimeError::InvalidCpu { cpu: owner_cpu });
    }
    ax_hal::console::set_input_irq_enabled(false);
    let (mut producer, consumer) = crate::serial::spsc::channel(RAW_RX_CAPACITY);
    let runtime = Arc::new(RawInputRuntime {
        available: SpinLock::new(Some(consumer)),
        overflow: AtomicBool::new(false),
        progress: WaitQueue::new(),
        poll_source: Arc::new(PollSet::new()),
        worker_signal: FixedIrqWorkerSignal::new(),
        irq_handle: OnceLock::new(),
    });
    let irq_runtime = runtime.clone();
    let request =
        ax_hal::irq::IrqRequest::new(move |_| handle_raw_input_irq(&mut producer, &irq_runtime))
            .share_mode(ax_hal::irq::ShareMode::Shared)
            .affinity(ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(
                owner_cpu,
            )))
            .auto_enable(ax_hal::irq::AutoEnable::No);
    let handle = match ax_hal::irq::request_irq(irq, request) {
        Ok(handle) => handle,
        Err(error) => {
            // Leave the device source masked: no handler owns this IRQ yet.
            // The cached initialization result makes this failure terminal;
            // no second IRQ owner may be registered over uncertain rollback.
            ax_hal::console::set_input_irq_enabled(false);
            return Err(error.into());
        }
    };
    ax_hal::console::set_input_irq_enabled(true);
    if let Err(error) = ax_hal::irq::enable_irq(handle) {
        ax_hal::console::set_input_irq_enabled(false);
        if let Err(free_error) = ax_hal::irq::free_irq(handle) {
            warn!("failed to release raw console IRQ after enable failure: {free_error:?}");
        }
        return Err(error.into());
    }
    let worker_runtime = runtime.clone();
    if let Err(error) = crate::task::spawn_raw_with_affinity(
        move || run_raw_input_worker(worker_runtime),
        alloc::format!("raw-console-{owner_cpu}-rx"),
        crate::task::default_task_stack_size(),
        affinity,
    ) {
        ax_hal::console::set_input_irq_enabled(false);
        if let Err(disable_error) = ax_hal::irq::disable_irq(handle) {
            warn!(
                "failed to disable raw console IRQ after worker spawn failure: {disable_error:?}"
            );
        }
        if let Err(free_error) = ax_hal::irq::free_irq(handle) {
            warn!("failed to release raw console IRQ after worker spawn failure: {free_error:?}");
        }
        return Err(error.into());
    }
    runtime.irq_handle.call_once(|| handle);
    Ok(runtime)
}

fn run_raw_input_worker(runtime: Arc<RawInputRuntime>) -> ! {
    loop {
        runtime
            .worker_signal
            .wait()
            .unwrap_or_else(|error| panic!("raw console IRQ waiter could not quiesce: {error}"));
        runtime.progress.notify_all();
        // SAFETY: the IRQ endpoint publishes queue and overflow state before
        // ringing the fixed worker. Readiness fanout runs here in task context
        // without holding a lock that a registered waker can re-enter.
        unsafe { runtime.poll_source.wake(IoEvents::IN) };
    }
}

fn handle_raw_input_irq(
    producer: &mut SpscProducer<RxItem>,
    runtime: &RawInputRuntime,
) -> ax_hal::irq::IrqReturn {
    let events = ax_hal::console::handle_irq();
    if events.is_empty() {
        return ax_hal::irq::IrqReturn::Unhandled;
    }

    let mut published = false;
    if events.contains(ax_hal::console::ConsoleIrqEvent::OVERRUN) {
        published |= publish_rx_item(producer, runtime, RxItem::Overrun);
    }

    published |= drain_raw_input(producer, runtime, ax_hal::console::read_bytes);

    if published || runtime.overflow.load(Ordering::Acquire) {
        runtime.worker_signal.notify();
    }
    ax_hal::irq::IrqReturn::Handled
}

fn drain_raw_input(
    producer: &mut SpscProducer<RxItem>,
    runtime: &RawInputRuntime,
    mut read: impl FnMut(&mut [u8]) -> usize,
) -> bool {
    let mut published = false;
    let mut bytes = [0u8; 64];
    let mut budget = RAW_RX_CAPACITY;
    while budget != 0 {
        let limit = bytes.len().min(budget);
        let count = read(&mut bytes[..limit]).min(limit);
        for &byte in &bytes[..count] {
            published |= publish_rx_item(
                producer,
                runtime,
                RxItem::Byte {
                    byte,
                    flag: RxFlag::Normal,
                },
            );
        }
        budget -= count;
        if count < limit {
            break;
        }
    }
    published
}

fn publish_rx_item(
    producer: &mut SpscProducer<RxItem>,
    runtime: &RawInputRuntime,
    item: RxItem,
) -> bool {
    if producer.push(item).is_ok() {
        true
    } else {
        runtime.overflow.store(true, Ordering::Release);
        false
    }
}

impl RawConsoleInput {
    pub(crate) fn try_read(&self, out: &mut [RxItem]) -> usize {
        if out.is_empty() {
            return 0;
        }
        let mut written = 0;
        if self.runtime.overflow.swap(false, Ordering::AcqRel) {
            out[written] = RxItem::Overrun;
            written += 1;
        }
        if let Some(consumer) = self.consumer.lock_irqsave().as_mut() {
            written += consumer.drain(&mut out[written..]);
        }
        written
    }

    pub(crate) fn wait_readable(&self) {
        self.runtime.progress.wait_until(|| self.has_pending());
    }

    pub(crate) fn discard_pending(&self) {
        self.runtime.overflow.store(false, Ordering::Release);
        if let Some(consumer) = self.consumer.lock_irqsave().as_mut() {
            consumer.clear();
        }
    }

    pub(crate) fn poll_source(&self) -> Arc<PollSet> {
        self.runtime.poll_source.clone()
    }

    fn has_pending(&self) -> bool {
        self.runtime.overflow.load(Ordering::Acquire)
            || self
                .consumer
                .lock_irqsave()
                .as_ref()
                .is_some_and(|consumer| !consumer.is_empty())
    }
}

impl Drop for RawConsoleInput {
    fn drop(&mut self) {
        let Some(consumer) = self.consumer.get_mut().take() else {
            return;
        };
        let mut available = self.runtime.available.lock_irqsave();
        debug_assert!(
            available.is_none(),
            "raw console cannot have two RX consumers"
        );
        if available.is_none() {
            *available = Some(consumer);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn raw_input_initialization_failure_is_terminal() {
        let attempts = AtomicUsize::new(0);
        let cell = OnceLock::new();
        for _ in 0..2 {
            assert_eq!(
                get_or_initialize_result(&cell, || {
                    attempts.fetch_add(1, Ordering::Relaxed);
                    Err::<(), _>(RuntimeError::OperationNotSupported)
                }),
                Err(RuntimeError::OperationNotSupported)
            );
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn raw_irq_queue_preserves_bytes_and_reports_overflow() {
        let (mut producer, consumer) = crate::serial::spsc::channel(1);
        let runtime = Arc::new(RawInputRuntime {
            available: SpinLock::new(None),
            overflow: AtomicBool::new(false),
            progress: WaitQueue::new(),
            poll_source: Arc::new(PollSet::new()),
            worker_signal: FixedIrqWorkerSignal::new(),
            irq_handle: OnceLock::new(),
        });
        let input = RawConsoleInput {
            consumer: SpinLock::new(Some(consumer)),
            runtime: runtime.clone(),
        };
        let retained = RxItem::Byte {
            byte: b'a',
            flag: RxFlag::Normal,
        };
        let dropped = RxItem::Byte {
            byte: b'b',
            flag: RxFlag::Normal,
        };

        assert!(publish_rx_item(&mut producer, &runtime, retained));
        assert!(!publish_rx_item(&mut producer, &runtime, dropped));

        let mut out = [RxItem::default(); 2];
        assert_eq!(input.try_read(&mut out), 2);
        assert_eq!(out, [RxItem::Overrun, retained]);
        assert_eq!(input.try_read(&mut out), 0);
    }

    #[test]
    fn raw_irq_drain_has_a_fixed_per_interrupt_budget() {
        let source_bytes = RAW_RX_CAPACITY + 64;
        let (mut producer, _consumer) = crate::serial::spsc::channel(source_bytes);
        let runtime = RawInputRuntime {
            available: SpinLock::new(None),
            overflow: AtomicBool::new(false),
            progress: WaitQueue::new(),
            poll_source: Arc::new(PollSet::new()),
            worker_signal: FixedIrqWorkerSignal::new(),
            irq_handle: OnceLock::new(),
        };
        let mut remaining = source_bytes;
        let mut drained = 0;

        drain_raw_input(&mut producer, &runtime, |out| {
            let count = out.len().min(remaining);
            out[..count].fill(b'x');
            remaining -= count;
            drained += count;
            count
        });

        assert_eq!(drained, RAW_RX_CAPACITY);
        assert_eq!(remaining, 64);
    }
}
