//! UART runtime ownership and task-context data service.
//!
//! Each UART has one CPU-affine maintenance task. Other CPUs submit bounded TX
//! chunks; only the IRQ endpoint and the maintenance task touch UART registers.

mod control;
mod ingress;
mod spsc;
mod state;
mod worker;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_driver::serial::SerialDevice;
pub use ax_driver::serial::SerialDeviceInfo;
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_task::{AxCpuMask, IrqNotify, TaskInner, WaitQueue};
use axpoll::{IoEvents, PollSet};
pub use rdif_serial::{Config, ConfigError, DataBits, Parity, RxFlag, StopBits};
use spin::Once;
pub use state::SerialStats;

use self::{
    control::{ControlOp, ControlQueue},
    ingress::TxIngress,
    spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
    state::{SerialIrqLatch, SerialStatsAtomic},
    worker::SerialWorker,
};

const NO_ACTIVE_CONSOLE: usize = usize::MAX;
const PANIC_TX_READY_SPINS: usize = 100_000;
const IRQ_RX_CAPACITY: usize = 16_384;
const SUBSCRIPTION_RX_CAPACITY: usize = 4_096;

static SERIAL_RUNTIMES: Once<Box<[SerialRuntimeHandle]>> = Once::new();
static ACTIVE_CONSOLE: AtomicUsize = AtomicUsize::new(NO_ACTIVE_CONSOLE);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxItem {
    Byte { byte: u8, flag: RxFlag },
    Overrun,
}

impl Default for RxItem {
    fn default() -> Self {
        Self::Byte {
            byte: 0,
            flag: RxFlag::Normal,
        }
    }
}

struct RuntimeIrqBridge {
    latch: SerialIrqLatch,
    rx_overflow: AtomicBool,
    notify: IrqNotify,
}

impl RuntimeIrqBridge {
    const fn new() -> Self {
        Self {
            latch: SerialIrqLatch::new(),
            rx_overflow: AtomicBool::new(false),
            notify: IrqNotify::new(),
        }
    }
}

struct RuntimeShared {
    index: usize,
    info: SerialDeviceInfo,
    owner_cpu: usize,
    polling: bool,
    port: SpinNoIrq<Box<dyn rdif_serial::UartPort>>,
    ingress: TxIngress,
    rx_subscription: SpinNoIrq<Option<SpscConsumer<RxItem>>>,
    control: ControlQueue,
    bridge: Arc<RuntimeIrqBridge>,
    stats: Arc<SerialStatsAtomic>,
    rx_source: Arc<PollSet>,
    tx_source: Arc<PollSet>,
    tx_progress: WaitQueue,
    started: AtomicBool,
    irq_handle: Once<ax_hal::irq::IrqHandle>,
}

impl RuntimeShared {
    fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    fn set_started(&self, started: bool) {
        self.started.store(started, Ordering::Release);
        if !started {
            self.tx_progress.notify_all(true);
        }
    }

    fn publish_tx_space(&self) {
        self.tx_progress.notify_all(true);
        // SAFETY: the maintenance task publishes queue space before waking
        // task-context poll waiters.
        unsafe { self.tx_source.wake(IoEvents::OUT) };
    }

    fn publish_tx_idle(&self) {
        self.tx_progress.notify_all(true);
        // SAFETY: idle is published under the TX queue lock before this wake.
        unsafe { self.tx_source.wake(IoEvents::OUT) };
    }
}

/// Cloneable OS-facing façade for one UART runtime.
#[derive(Clone)]
pub struct SerialRuntimeHandle {
    shared: Arc<RuntimeShared>,
}

impl SerialRuntimeHandle {
    pub fn info(&self) -> &SerialDeviceInfo {
        &self.shared.info
    }

    pub fn tx_sender(&self) -> SerialTxSender {
        SerialTxSender {
            shared: self.shared.clone(),
        }
    }

    /// Takes the only RX subscription. Starry serializes its readers above it.
    pub fn take_rx_subscription(&self) -> Option<SerialRxSubscription> {
        let consumer = self.shared.rx_subscription.lock().take()?;
        Some(SerialRxSubscription {
            consumer: SpinNoIrq::new(consumer),
            bridge: self.shared.bridge.clone(),
            source: self.shared.rx_source.clone(),
        })
    }

    pub fn start(&self, config: Config) -> AxResult {
        self.shared
            .control
            .submit(ControlOp::Start(config), &self.shared.bridge.notify)
    }

    pub fn shutdown(&self) -> AxResult {
        let result = self
            .shared
            .control
            .submit(ControlOp::Shutdown, &self.shared.bridge.notify);
        if result.is_ok() {
            let _ = ACTIVE_CONSOLE.compare_exchange(
                self.shared.index,
                NO_ACTIVE_CONSOLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        result
    }

    pub fn set_config(&self, config: Config) -> AxResult {
        self.shared
            .control
            .submit(ControlOp::SetConfig(config), &self.shared.bridge.notify)
    }

    pub fn activate_console_output(&self) -> AxResult {
        if !self.shared.started() {
            return Err(AxError::BadState);
        }
        ACTIVE_CONSOLE.store(self.shared.index, Ordering::Release);
        Ok(())
    }

    pub fn stats(&self) -> SerialStats {
        self.shared.stats.snapshot()
    }
}

/// Cloneable, bounded MPSC submission façade. It never accesses UART registers.
#[derive(Clone)]
pub struct SerialTxSender {
    shared: Arc<RuntimeShared>,
}

impl SerialTxSender {
    pub fn try_write(&self, bytes: &[u8]) -> AxResult<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if !self.shared.started() {
            return Err(AxError::BadState);
        }
        let accepted = self
            .shared
            .ingress
            .try_write(bytes, &self.shared.bridge.notify);
        if accepted == 0 {
            Err(AxError::WouldBlock)
        } else {
            Ok(accepted)
        }
    }

    pub fn wait_writable(&self) -> AxResult {
        if !self.shared.started() {
            return Err(AxError::BadState);
        }
        self.shared
            .tx_progress
            .wait_until(|| self.shared.ingress.write_room() > 0 || !self.shared.started());
        self.shared.started().then_some(()).ok_or(AxError::BadState)
    }

    pub fn wait_idle(&self) -> AxResult {
        if !self.shared.started() {
            return Err(AxError::BadState);
        }
        self.shared
            .tx_progress
            .wait_until(|| self.shared.ingress.is_idle() || !self.shared.started());
        if self.shared.ingress.is_idle() {
            Ok(())
        } else {
            Err(AxError::BadState)
        }
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        self.shared.tx_source.clone()
    }
}

/// The unique RX consumer for one UART runtime.
pub struct SerialRxSubscription {
    consumer: SpinNoIrq<SpscConsumer<RxItem>>,
    bridge: Arc<RuntimeIrqBridge>,
    source: Arc<PollSet>,
}

impl SerialRxSubscription {
    pub fn drain(&self, out: &mut [RxItem]) -> usize {
        let count = self.consumer.lock().drain(out);
        notify_drained_space(count, || self.bridge.notify.notify());
        count
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        self.source.clone()
    }
}

fn notify_drained_space(count: usize, notify_space: impl FnOnce()) {
    if count != 0 {
        notify_space();
    }
}

pub fn runtimes() -> &'static [SerialRuntimeHandle] {
    SERIAL_RUNTIMES.get().map_or(&[], Box::as_ref)
}

pub(crate) fn init(primary_cpu: usize) {
    let mut handles = Vec::new();
    for serial in ax_driver::serial::take_serial_devices() {
        match build_runtime(handles.len(), primary_cpu, serial) {
            Ok(handle) => handles.push(handle),
            Err(err) => warn!("failed to initialize serial runtime: {err:?}"),
        }
    }
    SERIAL_RUNTIMES.call_once(|| handles.into_boxed_slice());
}

fn build_runtime(
    index: usize,
    primary_cpu: usize,
    serial: SerialDevice,
) -> AxResult<SerialRuntimeHandle> {
    let SerialDevice {
        info,
        mut port,
        mut irq,
    } = serial;
    port.mask_all();

    let polling = info.irq.is_none();
    let bridge = Arc::new(RuntimeIrqBridge::new());
    let stats = Arc::new(SerialStatsAtomic::new());
    let (irq_rx_producer, irq_rx_consumer) = spsc::channel(IRQ_RX_CAPACITY);
    let (rx_output_producer, rx_output_consumer) = spsc::channel(SUBSCRIPTION_RX_CAPACITY);
    let shared = Arc::new(RuntimeShared {
        index,
        info,
        owner_cpu: primary_cpu,
        polling,
        port: SpinNoIrq::new(port),
        ingress: TxIngress::new(),
        rx_subscription: SpinNoIrq::new(Some(rx_output_consumer)),
        control: ControlQueue::new(),
        bridge: bridge.clone(),
        stats: stats.clone(),
        rx_source: Arc::new(PollSet::new()),
        tx_source: Arc::new(PollSet::new()),
        tx_progress: WaitQueue::new(),
        started: AtomicBool::new(false),
        irq_handle: Once::new(),
    });

    let worker = SerialWorker::new(shared.clone(), irq_rx_consumer, rx_output_producer);
    let task = TaskInner::new(
        move || worker.run(),
        alloc::format!("serial{index}-maint"),
        ax_task::default_task_stack_size(),
    );
    task.set_cpumask(AxCpuMask::one_shot(primary_cpu));

    if let Some(binding) = shared.info.irq.clone() {
        let irq_id = crate::irq::resolve_binding_irq(binding).map_err(|err| {
            warn!(
                "failed to resolve serial IRQ for {}: {err:?}",
                shared.info.name
            );
            AxError::Unsupported
        })?;
        let callback_bridge = bridge;
        let callback_stats = stats;
        let mut callback_rx = RuntimeIrqRxSink {
            producer: irq_rx_producer,
            bridge: callback_bridge.clone(),
            stats: callback_stats.clone(),
        };
        let request = ax_hal::irq::IrqRequest::new(move |_| {
            let Some(event) = irq.handle(&mut callback_rx) else {
                callback_stats.spurious_irq();
                return ax_hal::irq::IrqReturn::Unhandled;
            };
            callback_stats.handled_irq(event);
            callback_bridge.latch.publish(event);
            callback_bridge.notify.notify_irq();
            ax_hal::irq::IrqReturn::Handled
        })
        .share_mode(ax_hal::irq::ShareMode::Shared)
        .affinity(ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(
            primary_cpu,
        )))
        .auto_enable(ax_hal::irq::AutoEnable::Yes);
        let handle = ax_hal::irq::request_irq(irq_id, request).map_err(|err| {
            warn!(
                "failed to register serial IRQ for {}: {err:?}",
                shared.info.name
            );
            AxError::Unsupported
        })?;
        shared.irq_handle.call_once(|| handle);
    }

    ax_task::spawn_task(task);
    info!(
        "serial runtime {} ready: cpu={}, irq={:?}, polling={}",
        shared.info.name, shared.owner_cpu, shared.info.irq, shared.polling
    );
    Ok(SerialRuntimeHandle { shared })
}

struct RuntimeIrqRxSink {
    producer: SpscProducer<rdif_serial::RxSample>,
    bridge: Arc<RuntimeIrqBridge>,
    stats: Arc<SerialStatsAtomic>,
}

impl rdif_serial::IrqRxSink for RuntimeIrqRxSink {
    fn push(&mut self, sample: rdif_serial::RxSample) {
        if self.producer.push(sample).is_err() {
            self.stats.add_rx_dropped(1);
            self.bridge.rx_overflow.store(true, Ordering::Release);
        }
    }
}

/// Routes normal logs through the bounded TX channel. Panic output only takes
/// the port after a successful non-blocking gate acquisition.
pub(crate) fn route_console_bytes(bytes: &[u8]) -> Option<usize> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    if axpanic::oops_in_progress() {
        let Some(mut port) = runtime.shared.port.try_lock() else {
            runtime.shared.stats.add_log_dropped(bytes.len());
            return Some(0);
        };
        port.mask_all();
        let mut written = 0;
        let mut spins = 0;
        while written < bytes.len() && spins < PANIC_TX_READY_SPINS {
            let count = port.write_tx(&bytes[written..]);
            if count == 0 {
                spins += 1;
                core::hint::spin_loop();
            } else {
                written += count;
                spins = 0;
            }
        }
        runtime.shared.stats.add_log_dropped(bytes.len() - written);
        return Some(written);
    }

    let accepted = runtime
        .shared
        .ingress
        .try_write_log(bytes, &runtime.shared.bridge.notify);
    runtime.shared.stats.add_log_dropped(bytes.len() - accepted);
    Some(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_sink_drops_only_after_the_preallocated_ring_is_full() {
        let bridge = Arc::new(RuntimeIrqBridge::new());
        let stats = Arc::new(SerialStatsAtomic::new());
        let (producer, mut consumer) = spsc::channel(2);
        let mut sink = RuntimeIrqRxSink {
            producer,
            bridge: bridge.clone(),
            stats: stats.clone(),
        };
        let samples = [
            rdif_serial::RxSample {
                byte: Some(1),
                ..rdif_serial::RxSample::default()
            },
            rdif_serial::RxSample {
                byte: Some(2),
                ..rdif_serial::RxSample::default()
            },
            rdif_serial::RxSample {
                byte: Some(3),
                ..rdif_serial::RxSample::default()
            },
        ];
        for sample in samples {
            rdif_serial::IrqRxSink::push(&mut sink, sample);
        }

        assert_eq!(consumer.pop().and_then(|sample| sample.byte), Some(1));
        assert_eq!(consumer.pop().and_then(|sample| sample.byte), Some(2));
        assert!(consumer.pop().is_none());
        assert_eq!(stats.snapshot().rx_dropped, 1);
        assert!(bridge.rx_overflow.load(Ordering::Acquire));
    }

    #[test]
    fn subscription_drain_notifies_a_worker_waiting_for_output_space() {
        let (mut producer, consumer) = spsc::channel(1);
        producer.push(RxItem::Overrun).unwrap();
        let mut consumer = consumer;
        let mut item = [RxItem::default()];
        let mut notify_count = 0;

        let count = consumer.drain(&mut item);
        notify_drained_space(count, || notify_count += 1);
        assert_eq!(count, 1);
        assert_eq!(item, [RxItem::Overrun]);
        assert_eq!(notify_count, 1);
    }
}
