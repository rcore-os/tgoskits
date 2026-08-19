//! IRQ-backed task input for a HAL-owned console without a runtime UART.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use ax_lazyinit::OnceLock;
use ax_task::WaitQueue;
use axpoll::{IoEvents, PollSet};

use crate::{
    RuntimeError, RuntimeResult,
    serial::{
        RxFlag, RxItem,
        spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
    },
    sync::SpinLock,
};

const RAW_RX_CAPACITY: usize = 4_096;

static RAW_INPUT: OnceLock<Result<Arc<RawInputRuntime>, RuntimeError>> = OnceLock::new();

struct RawInputRuntime {
    available: SpinLock<Option<SpscConsumer<RxItem>>>,
    overflow: AtomicBool,
    progress: WaitQueue,
    poll_source: Arc<PollSet>,
    irq_handle: OnceLock<ax_hal::irq::IrqHandle>,
}

/// Unique task-side consumer for the HAL console's IRQ-backed RX queue.
pub(crate) struct RawConsoleInput {
    consumer: SpinLock<Option<SpscConsumer<RxItem>>>,
    runtime: Arc<RawInputRuntime>,
}

pub(crate) fn take_input() -> RuntimeResult<RawConsoleInput> {
    let runtime = RAW_INPUT
        .call_once(init_raw_input)
        .as_ref()
        .map_err(|error| *error)?
        .clone();
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

fn init_raw_input() -> RuntimeResult<Arc<RawInputRuntime>> {
    let irq = ax_hal::console::irq_num().ok_or(RuntimeError::OperationNotSupported)?;
    ax_hal::console::set_input_irq_enabled(false);
    let (mut producer, consumer) = crate::serial::spsc::channel(RAW_RX_CAPACITY);
    let runtime = Arc::new(RawInputRuntime {
        available: SpinLock::new(Some(consumer)),
        overflow: AtomicBool::new(false),
        progress: WaitQueue::new(),
        poll_source: Arc::new(PollSet::new()),
        irq_handle: OnceLock::new(),
    });
    let irq_runtime = runtime.clone();
    let request =
        ax_hal::irq::IrqRequest::new(move |_| handle_raw_input_irq(&mut producer, &irq_runtime))
            .share_mode(ax_hal::irq::ShareMode::Shared)
            .auto_enable(ax_hal::irq::AutoEnable::No);
    let handle = ax_hal::irq::request_irq(irq, request)?;
    ax_hal::console::set_input_irq_enabled(true);
    if let Err(error) = ax_hal::irq::enable_irq(handle) {
        ax_hal::console::set_input_irq_enabled(false);
        if let Err(free_error) = ax_hal::irq::free_irq(handle) {
            warn!("failed to release raw console IRQ after enable failure: {free_error:?}");
        }
        return Err(error.into());
    }
    runtime.irq_handle.call_once(|| handle);
    Ok(runtime)
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

    let mut bytes = [0u8; 64];
    loop {
        let count = ax_hal::console::read_bytes(&mut bytes);
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
        if count < bytes.len() {
            break;
        }
    }

    if published || runtime.overflow.load(Ordering::Acquire) {
        runtime.progress.notify_all_from_irq();
        runtime.poll_source.wake_from_irq(IoEvents::IN);
    }
    ax_hal::irq::IrqReturn::Handled
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
    use super::*;

    #[test]
    fn raw_irq_queue_preserves_bytes_and_reports_overflow() {
        let (mut producer, consumer) = crate::serial::spsc::channel(1);
        let runtime = Arc::new(RawInputRuntime {
            available: SpinLock::new(None),
            overflow: AtomicBool::new(false),
            progress: WaitQueue::new(),
            poll_source: Arc::new(PollSet::new()),
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
}
