//! IRQ-backed task input for a HAL-owned console without a runtime UART.

use alloc::sync::Arc;
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

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
const RAW_POLL_INTERVAL: Duration = Duration::from_millis(10);

static RAW_INPUT: OnceLock<Arc<RawInputRuntime>> = OnceLock::new();

struct RawInputRuntime {
    available: SpinLock<Option<SpscConsumer<RxItem>>>,
    /// Serializes UART RX status/FIFO access and publication from IRQ and polling paths.
    producer: SpinLock<SpscProducer<RxItem>>,
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
    let runtime = RAW_INPUT.get_or_try_init(init_raw_input)?.clone();
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
    let (producer, consumer) = crate::serial::spsc::channel(RAW_RX_CAPACITY);
    let runtime = Arc::new(RawInputRuntime {
        available: SpinLock::new(Some(consumer)),
        producer: SpinLock::new(producer),
        overflow: AtomicBool::new(false),
        progress: WaitQueue::new(),
        poll_source: Arc::new(PollSet::new()),
        irq_handle: OnceLock::new(),
    });
    let irq_runtime = runtime.clone();
    let request = ax_hal::irq::IrqRequest::new(move |_| handle_raw_input_irq(&irq_runtime))
        .share_mode(ax_hal::irq::ShareMode::Shared)
        .auto_enable(ax_hal::irq::AutoEnable::No);
    let handle = activate_raw_input_irq(
        ax_hal::console::set_input_irq_enabled,
        || ax_hal::irq::request_irq(irq, request),
        ax_hal::irq::enable_irq,
        |handle| {
            if let Err(free_error) = ax_hal::irq::free_irq(handle) {
                warn!("failed to release raw console IRQ after enable failure: {free_error:?}");
            }
        },
    )?;
    runtime.irq_handle.call_once(|| handle);
    Ok(runtime)
}

fn activate_raw_input_irq<H: Copy, E>(
    mut set_source_enabled: impl FnMut(bool),
    request_line: impl FnOnce() -> Result<H, E>,
    enable_line: impl FnOnce(H) -> Result<(), E>,
    release_line: impl FnOnce(H),
) -> Result<H, E> {
    set_source_enabled(false);
    let handle = request_line()?;
    if let Err(error) = enable_line(handle) {
        set_source_enabled(false);
        release_line(handle);
        return Err(error);
    }
    set_source_enabled(true);
    Ok(handle)
}

fn handle_raw_input_irq(runtime: &RawInputRuntime) -> ax_hal::irq::IrqReturn {
    let (irq_return, published) = service_raw_input_irq_with(
        runtime,
        ax_hal::console::handle_irq,
        ax_hal::console::read_bytes,
    );

    if published || runtime.overflow.load(Ordering::Acquire) {
        runtime.progress.notify_all_from_irq();
        runtime.poll_source.wake_from_irq(IoEvents::IN);
    }
    irq_return
}

fn service_raw_input_irq_with(
    runtime: &RawInputRuntime,
    handle_irq: impl FnOnce() -> ax_hal::console::ConsoleIrqEvent,
    read: impl FnMut(&mut [u8]) -> usize,
) -> (ax_hal::irq::IrqReturn, bool) {
    let mut producer = runtime.producer.lock_irqsave();
    let events = handle_irq();
    if events.is_empty() {
        return (ax_hal::irq::IrqReturn::Unhandled, false);
    }

    let mut published = false;
    if events.contains(ax_hal::console::ConsoleIrqEvent::OVERRUN) {
        published |= publish_rx_item(&mut producer, runtime, RxItem::Overrun);
    }

    published |= poll_raw_input(&mut producer, runtime, read);
    (ax_hal::irq::IrqReturn::Handled, published)
}

fn poll_raw_input(
    producer: &mut SpscProducer<RxItem>,
    runtime: &RawInputRuntime,
    read: impl FnMut(&mut [u8]) -> usize,
) -> bool {
    drain_raw_input(producer, runtime, read)
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
    pub(crate) const fn recovery_poll_interval() -> Duration {
        RAW_POLL_INTERVAL
    }

    pub(crate) fn try_read(&self, out: &mut [RxItem]) -> usize {
        self.try_read_with(out, ax_hal::console::read_bytes)
    }

    fn try_read_with(&self, out: &mut [RxItem], read: impl FnMut(&mut [u8]) -> usize) -> usize {
        if out.is_empty() {
            return 0;
        }
        poll_raw_input(
            &mut self.runtime.producer.lock_irqsave(),
            &self.runtime,
            read,
        );
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
        self.runtime
            .progress
            .wait_timeout_until(RAW_POLL_INTERVAL, || self.has_pending());
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
    use alloc::{rc::Rc, vec::Vec};
    use core::cell::{Cell, RefCell};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ActivationStep {
        MaskSource,
        RequestLine,
        EnableLine,
        UnmaskSource,
        ReleaseLine,
    }

    #[test]
    fn raw_input_enables_controller_before_uart_source() {
        let steps = Rc::new(RefCell::new(Vec::new()));
        let source_steps = steps.clone();
        let request_steps = steps.clone();
        let enable_steps = steps.clone();

        activate_raw_input_irq(
            move |enabled| {
                source_steps.borrow_mut().push(if enabled {
                    ActivationStep::UnmaskSource
                } else {
                    ActivationStep::MaskSource
                });
            },
            move || {
                request_steps.borrow_mut().push(ActivationStep::RequestLine);
                Ok::<_, ()>(7usize)
            },
            move |_| {
                enable_steps.borrow_mut().push(ActivationStep::EnableLine);
                Ok::<_, ()>(())
            },
            |_| {},
        )
        .unwrap();

        assert_eq!(
            *steps.borrow(),
            [
                ActivationStep::MaskSource,
                ActivationStep::RequestLine,
                ActivationStep::EnableLine,
                ActivationStep::UnmaskSource,
            ]
        );
    }

    #[test]
    fn raw_input_enable_failure_masks_source_and_releases_line() {
        let steps = Rc::new(RefCell::new(Vec::new()));
        let source_steps = steps.clone();
        let request_steps = steps.clone();
        let enable_steps = steps.clone();
        let release_steps = steps.clone();

        assert_eq!(
            activate_raw_input_irq(
                move |enabled| {
                    source_steps.borrow_mut().push(if enabled {
                        ActivationStep::UnmaskSource
                    } else {
                        ActivationStep::MaskSource
                    });
                },
                move || {
                    request_steps.borrow_mut().push(ActivationStep::RequestLine);
                    Ok::<_, &'static str>(7usize)
                },
                move |_| {
                    enable_steps.borrow_mut().push(ActivationStep::EnableLine);
                    Err("enable failed")
                },
                move |_| release_steps.borrow_mut().push(ActivationStep::ReleaseLine),
            ),
            Err("enable failed")
        );
        assert_eq!(
            *steps.borrow(),
            [
                ActivationStep::MaskSource,
                ActivationStep::RequestLine,
                ActivationStep::EnableLine,
                ActivationStep::MaskSource,
                ActivationStep::ReleaseLine,
            ]
        );
    }

    #[test]
    fn raw_irq_queue_preserves_bytes_and_reports_overflow() {
        let (producer, consumer) = crate::serial::spsc::channel(1);
        let runtime = Arc::new(RawInputRuntime {
            available: SpinLock::new(None),
            producer: SpinLock::new(producer),
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

        {
            let mut producer = runtime.producer.lock_irqsave();
            assert!(publish_rx_item(&mut producer, &runtime, retained));
            assert!(!publish_rx_item(&mut producer, &runtime, dropped));
        }

        let mut out = [RxItem::default(); 2];
        assert_eq!(input.try_read_with(&mut out, |_| 0), 2);
        assert_eq!(out, [RxItem::Overrun, retained]);
        assert_eq!(input.try_read_with(&mut out, |_| 0), 0);
    }

    #[test]
    fn raw_irq_drain_has_a_fixed_per_interrupt_budget() {
        let source_bytes = RAW_RX_CAPACITY + 64;
        let (producer, _consumer) = crate::serial::spsc::channel(source_bytes);
        let runtime = RawInputRuntime {
            available: SpinLock::new(None),
            producer: SpinLock::new(producer),
            overflow: AtomicBool::new(false),
            progress: WaitQueue::new(),
            poll_source: Arc::new(PollSet::new()),
            irq_handle: OnceLock::new(),
        };
        let mut remaining = source_bytes;
        let mut drained = 0;

        poll_raw_input(&mut runtime.producer.lock_irqsave(), &runtime, |out| {
            let count = out.len().min(remaining);
            out[..count].fill(b'x');
            remaining -= count;
            drained += count;
            count
        });

        assert_eq!(drained, RAW_RX_CAPACITY);
        assert_eq!(remaining, 64);
    }

    #[test]
    #[cfg(feature = "smp")]
    fn raw_irq_status_probe_holds_the_uart_producer_lock() {
        let (producer, _consumer) = crate::serial::spsc::channel(4);
        let runtime = RawInputRuntime {
            available: SpinLock::new(None),
            producer: SpinLock::new(producer),
            overflow: AtomicBool::new(false),
            progress: WaitQueue::new(),
            poll_source: Arc::new(PollSet::new()),
            irq_handle: OnceLock::new(),
        };
        let status_was_serialized = Cell::new(false);

        let (irq_return, _) = service_raw_input_irq_with(
            &runtime,
            || {
                status_was_serialized.set(runtime.producer.try_lock_irqsave().is_none());
                ax_hal::console::ConsoleIrqEvent::RX_READY
            },
            |_| 0,
        );

        assert_eq!(irq_return, ax_hal::irq::IrqReturn::Handled);
        assert!(
            status_was_serialized.get(),
            "UART status must be sampled while the producer lock excludes recovery polling"
        );
    }

    #[test]
    fn raw_polling_recovers_a_byte_without_an_irq_notification() {
        assert_eq!(RawConsoleInput::recovery_poll_interval(), RAW_POLL_INTERVAL);
        let (producer, mut consumer) = crate::serial::spsc::channel(4);
        let runtime = RawInputRuntime {
            available: SpinLock::new(None),
            producer: SpinLock::new(producer),
            overflow: AtomicBool::new(false),
            progress: WaitQueue::new(),
            poll_source: Arc::new(PollSet::new()),
            irq_handle: OnceLock::new(),
        };
        let mut supplied = false;

        assert!(poll_raw_input(
            &mut runtime.producer.lock_irqsave(),
            &runtime,
            |out| {
                if supplied {
                    return 0;
                }
                out[0] = b'x';
                supplied = true;
                1
            }
        ));

        assert_eq!(
            consumer.pop(),
            Some(RxItem::Byte {
                byte: b'x',
                flag: RxFlag::Normal,
            })
        );
    }
}
