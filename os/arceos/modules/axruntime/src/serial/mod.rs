//! UART runtime ownership and task-context data service.
//!
//! Each UART has one CPU-affine maintenance task. Other CPUs submit bounded TX
//! chunks; only the IRQ endpoint and the maintenance task touch UART registers.

mod control;
mod ingress;
mod spsc;
mod state;
mod worker;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_driver::serial::SerialDevice;
pub use ax_driver::serial::SerialDeviceInfo;
use ax_errno::{AxError, AxResult};
use ax_sync::PiMutex;
use axpoll::{IoEvents, PollSet};
pub use rdif_serial::{Config, ConfigError, DataBits, Parity, RxFlag, StopBits, UartRegisterGate};
use spin::Once;
pub use state::SerialStats;

use self::{
    control::{ControlOp, ControlQueue},
    ingress::TxIngress,
    spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
    state::{SerialIrqLatch, SerialStatsAtomic},
    worker::SerialWorker,
};
use crate::task::{
    CpuId, CpuSet, IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, ThreadHandle, ThreadId,
    WaitQueue, quiesce_irq_wait,
};

const NO_ACTIVE_CONSOLE: usize = usize::MAX;
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
    register_retry: AtomicBool,
    doorbell: IrqWaitCell,
    park: WaitQueue,
    waiter: Once<SerialWorkerWaiter>,
}

impl RuntimeIrqBridge {
    const fn new() -> Self {
        Self {
            latch: SerialIrqLatch::new(),
            rx_overflow: AtomicBool::new(false),
            register_retry: AtomicBool::new(false),
            doorbell: IrqWaitCell::new(),
            park: WaitQueue::new(),
            waiter: Once::new(),
        }
    }

    fn notify(&self) {
        let _result = self.doorbell.notify();
    }

    fn take_register_retry(&self) -> bool {
        self.register_retry.swap(false, Ordering::AcqRel)
    }

    fn wait(&self) {
        let current = crate::task::current_thread_handle().unwrap_or_else(|error| {
            panic!("serial maintenance worker has no scheduler thread: {error}")
        });
        let waiter = self
            .waiter
            .call_once(|| create_serial_worker_waiter(&current));
        assert_eq!(
            waiter.owner,
            current.id(),
            "serial notifications must be consumed by one fixed maintenance thread"
        );

        match self.doorbell.register(&waiter.registration) {
            IrqRegisterResult::ConsumedPending => {}
            IrqRegisterResult::Registered(token)
            | IrqRegisterResult::NotificationInFlight(token) => {
                // Registration ownership is the single event predicate. The
                // notifier releases it before the direct scheduler wake, and
                // an event coalesced before registration releases it
                // synchronously without leaving a stale self-wake behind.
                self.park.wait_until(|| !token.is_attached());
                quiesce_irq_wait(token)
                    .unwrap_or_else(|error| panic!("serial IRQ waiter could not quiesce: {error}"));
            }
            IrqRegisterResult::Occupied => {
                panic!("serial IRQ waiter was registered concurrently")
            }
        }
    }
}

struct SerialWorkerWaiter {
    owner: ThreadId,
    registration: IrqWaitRegistration,
}

struct PendingIrqRegistration {
    handle: ax_hal::irq::IrqHandle,
    device_name: String,
    committed: bool,
}

impl PendingIrqRegistration {
    fn new(handle: ax_hal::irq::IrqHandle, device_name: String) -> Self {
        Self {
            handle,
            device_name,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PendingIrqRegistration {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Err(error) = ax_hal::irq::free_irq(self.handle) {
            warn!(
                "failed to roll back serial IRQ registration for {}: {error:?}",
                self.device_name
            );
        }
    }
}

fn create_serial_worker_waiter(current: &ThreadHandle) -> SerialWorkerWaiter {
    SerialWorkerWaiter {
        owner: current.id(),
        registration: IrqWaitRegistration::new(current.wake_handle()),
    }
}

fn try_enter_irq_registers<'a, E: ?Sized>(
    gate: &'a UartRegisterGate<E>,
    bridge: &RuntimeIrqBridge,
) -> Option<rdif_serial::UartRegisterGuard<'a, E>> {
    let guard = gate.try_enter();
    if guard.is_none() {
        // Emergency TX masks every device source before touching the FIFO, so a
        // level-triggered line cannot continuously reassert while the IRQ
        // endpoint defers register access. Publish the retry before waking the
        // fixed worker; it polls status and restores normal source ownership
        // after the bounded emergency transaction releases the gate.
        bridge.register_retry.store(true, Ordering::Release);
        bridge.notify();
    }
    guard
}

struct RuntimeShared {
    index: usize,
    info: SerialDeviceInfo,
    owner_cpu: usize,
    polling: bool,
    register_gate: Arc<UartRegisterGate>,
    ingress: TxIngress,
    rx_subscription: PiMutex<Option<SpscConsumer<RxItem>>>,
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
            self.tx_progress.notify_all();
        }
    }

    fn publish_tx_space(&self) {
        self.tx_progress.notify_all();
        // SAFETY: the maintenance task publishes queue space before waking
        // task-context poll waiters.
        unsafe { self.tx_source.wake(IoEvents::OUT) };
    }

    fn publish_tx_idle(&self) {
        self.tx_progress.notify_all();
        // SAFETY: idle is Release-published before this task-context wake.
        unsafe { self.tx_source.wake(IoEvents::OUT) };
    }

    fn enable_irq(&self) -> AxResult {
        let Some(handle) = self.irq_handle.get().copied() else {
            return Ok(());
        };
        ax_hal::irq::enable_irq(handle).map_err(|err| {
            warn!(
                "failed to enable serial IRQ for {}: {err:?}",
                self.info.name
            );
            AxError::Io
        })
    }

    fn disable_irq(&self) {
        let Some(handle) = self.irq_handle.get().copied() else {
            return;
        };
        if let Err(err) = ax_hal::irq::disable_irq(handle) {
            warn!(
                "failed to disable serial IRQ for {}: {err:?}",
                self.info.name
            );
        }
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
            consumer: PiMutex::new(consumer),
            bridge: self.shared.bridge.clone(),
            source: self.shared.rx_source.clone(),
        })
    }

    pub fn start(&self, config: Config) -> AxResult {
        self.shared
            .control
            .submit(ControlOp::Start(config), || self.shared.bridge.notify())
    }

    pub fn shutdown(&self) -> AxResult {
        let result = self
            .shared
            .control
            .submit(ControlOp::Shutdown, || self.shared.bridge.notify());
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
            .submit(ControlOp::SetConfig(config), || self.shared.bridge.notify())
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
            .try_write(bytes, || self.shared.bridge.notify());
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
    consumer: PiMutex<SpscConsumer<RxItem>>,
    bridge: Arc<RuntimeIrqBridge>,
    source: Arc<PollSet>,
}

impl SerialRxSubscription {
    pub fn drain(&self, out: &mut [RxItem]) -> usize {
        let count = self.consumer.lock().drain(out);
        notify_drained_space(count, || self.bridge.notify());
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
        register_gate,
    } = serial;
    port.mask_all();

    let polling = info.irq.is_none();
    let bridge = Arc::new(RuntimeIrqBridge::new());
    let stats = Arc::new(SerialStatsAtomic::new());
    let register_gate: Arc<UartRegisterGate> = Arc::from(register_gate);
    let (irq_rx_producer, irq_rx_consumer) = spsc::channel(IRQ_RX_CAPACITY);
    let (rx_output_producer, rx_output_consumer) = spsc::channel(SUBSCRIPTION_RX_CAPACITY);
    let shared = Arc::new(RuntimeShared {
        index,
        info,
        owner_cpu: primary_cpu,
        polling,
        register_gate: register_gate.clone(),
        ingress: TxIngress::new(),
        rx_subscription: PiMutex::new(Some(rx_output_consumer)),
        control: ControlQueue::new(),
        bridge: bridge.clone(),
        stats: stats.clone(),
        rx_source: Arc::new(PollSet::new()),
        tx_source: Arc::new(PollSet::new()),
        tx_progress: WaitQueue::new(),
        started: AtomicBool::new(false),
        irq_handle: Once::new(),
    });

    let worker = SerialWorker::new(
        shared.clone(),
        port,
        register_gate.clone(),
        irq_rx_consumer,
        rx_output_producer,
    );
    let owner_cpu = u32::try_from(primary_cpu).map_err(|_| AxError::InvalidInput)?;
    let mut affinity = CpuSet::empty(ax_hal::cpu_num());
    if !affinity.insert(CpuId::new(owner_cpu)) {
        return Err(AxError::InvalidInput);
    }

    let mut pending_irq_registration = None;
    if let Some(binding) = shared.info.irq.clone() {
        let irq_id = crate::irq::resolve_binding_irq(binding).map_err(|err| {
            warn!(
                "failed to resolve serial IRQ for {}: {err:?}",
                shared.info.name
            );
            AxError::Unsupported
        })?;
        let mut callback_publisher = RuntimeIrqPublisher {
            producer: irq_rx_producer,
            bridge,
            stats,
            register_gate,
        };
        let request = serial_irq_request(
            ax_hal::irq::IrqRequest::new(move |_| callback_publisher.handle(irq.as_mut())),
            primary_cpu,
        );
        let handle = ax_hal::irq::request_irq(irq_id, request).map_err(|err| {
            warn!(
                "failed to register serial IRQ for {}: {err:?}",
                shared.info.name
            );
            AxError::Unsupported
        })?;
        shared.irq_handle.call_once(|| handle);
        pending_irq_registration = Some(PendingIrqRegistration::new(
            handle,
            shared.info.name.clone(),
        ));
    }

    crate::task::spawn_raw_with_affinity(
        move || worker.run(),
        alloc::format!("serial{index}-maint"),
        crate::task::default_task_stack_size(),
        affinity,
    )
    .map_err(|error| {
        warn!(
            "failed to start serial maintenance worker for {}: {error}",
            shared.info.name
        );
        AxError::BadState
    })?;
    if let Some(registration) = pending_irq_registration {
        registration.commit();
    }
    info!(
        "serial runtime {} ready: cpu={}, irq={:?}, polling={}",
        shared.info.name, shared.owner_cpu, shared.info.irq, shared.polling
    );
    Ok(SerialRuntimeHandle { shared })
}

fn serial_irq_request(
    request: ax_hal::irq::IrqRequest,
    primary_cpu: usize,
) -> ax_hal::irq::IrqRequest {
    request
        .share_mode(ax_hal::irq::ShareMode::Shared)
        .affinity(ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(
            primary_cpu,
        )))
        .auto_enable(ax_hal::irq::AutoEnable::No)
}

/// IRQ-safe publication boundary captured beside the IRQ-owned driver endpoint.
///
/// The registered callback cannot reach the serial worker, control queue, or
/// device manager. It can only execute a bounded register transaction and
/// publish value reports into preallocated state.
struct RuntimeIrqPublisher {
    producer: SpscProducer<rdif_serial::RxSample>,
    bridge: Arc<RuntimeIrqBridge>,
    stats: Arc<SerialStatsAtomic>,
    register_gate: Arc<UartRegisterGate>,
}

impl RuntimeIrqPublisher {
    fn handle(&mut self, irq: &mut dyn rdif_serial::UartIrq) -> ax_hal::irq::IrqReturn {
        let report = {
            let Some(_register_access) = try_enter_irq_registers(&self.register_gate, &self.bridge)
            else {
                // Only emergency output can contend with the same-CPU
                // worker/IRQ serialization. Never wait in hard IRQ.
                return ax_hal::irq::IrqReturn::Handled;
            };
            irq.handle()
        };
        let Some(report) = report else {
            self.stats.spurious_irq();
            return ax_hal::irq::IrqReturn::Unhandled;
        };

        self.publish_batch(report.rx);
        self.stats.handled_irq(report.event);
        self.bridge.latch.publish(report.event);
        self.bridge.notify();
        ax_hal::irq::IrqReturn::Handled
    }

    fn publish_batch(&mut self, batch: rdif_serial::IrqRxBatch) {
        for &sample in batch.as_slice() {
            self.publish_sample(sample);
        }
    }

    fn publish_sample(&mut self, sample: rdif_serial::RxSample) {
        if self.producer.push(sample).is_err() {
            self.stats.add_rx_dropped(1);
            self.bridge.rx_overflow.store(true, Ordering::Release);
        }
    }
}

/// Routes normal logs through the lock-free bounded MPSC ring. Panic output
/// uses only the restricted emergency endpoint after a non-blocking gate.
pub(crate) fn route_console_bytes(bytes: &[u8]) -> Option<usize> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    if axpanic::oops_in_progress() {
        return Some(route_emergency_bytes(
            &runtime.shared.register_gate,
            &runtime.shared.stats,
            bytes,
            || runtime.shared.bridge.notify(),
        ));
    }

    let accepted = runtime
        .shared
        .ingress
        .try_write_log(bytes, || runtime.shared.bridge.notify());
    runtime.shared.stats.add_log_dropped(bytes.len() - accepted);
    Some(accepted)
}

fn route_emergency_bytes<E: rdif_serial::UartEmergencyTx + ?Sized>(
    register_gate: &UartRegisterGate<E>,
    stats: &SerialStatsAtomic,
    bytes: &[u8],
    notify_released: impl FnOnce(),
) -> usize {
    let Some(register_access) = register_gate.try_enter() else {
        stats.add_log_dropped(bytes.len());
        return 0;
    };
    let written = register_access.try_write(bytes);
    stats.add_log_dropped(bytes.len() - written);
    // Publish register availability before the IRQ-safe doorbell. A worker
    // that consumed the original TX notification while emergency output held
    // the gate must not sleep until an unrelated interrupt arrives.
    drop(register_access);
    notify_released();
    written
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    struct BoundedEmergencyTx;

    impl rdif_serial::UartEmergencyTx for BoundedEmergencyTx {
        unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize {
            bytes.len().min(2)
        }
    }

    #[test]
    fn irq_publisher_drops_only_after_the_preallocated_ring_is_full() {
        let bridge = Arc::new(RuntimeIrqBridge::new());
        let stats = Arc::new(SerialStatsAtomic::new());
        let (producer, mut consumer) = spsc::channel(2);
        let register_gate = Arc::new(UartRegisterGate::new(BoundedEmergencyTx));
        let mut publisher = RuntimeIrqPublisher {
            producer,
            bridge: bridge.clone(),
            stats: stats.clone(),
            register_gate,
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
        let mut batch = rdif_serial::IrqRxBatch::new();
        for sample in samples {
            batch.try_push(sample).unwrap();
        }
        publisher.publish_batch(batch);

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

    #[test]
    fn serial_irq_stays_disabled_until_the_worker_starts_the_port() {
        let request = serial_irq_request(
            ax_hal::irq::IrqRequest::new(|_| ax_hal::irq::IrqReturn::Handled),
            0,
        );

        assert_eq!(
            request.auto_enable_mode(),
            ax_hal::irq::AutoEnable::No,
            "the IRQ action must not run before the worker has configured the UART"
        );
    }

    #[test]
    fn serial_work_is_coalesced_by_the_irq_doorbell() {
        let bridge = RuntimeIrqBridge::new();

        bridge.notify();

        assert!(bridge.doorbell.is_pending());
    }

    #[test]
    fn irq_gate_conflict_is_published_for_task_context_retry() {
        let bridge = RuntimeIrqBridge::new();
        let gate = UartRegisterGate::new(());
        let _owner = gate.try_enter().expect("first register owner");

        assert!(try_enter_irq_registers(&gate, &bridge).is_none());
        assert!(
            bridge.take_register_retry(),
            "the hard-IRQ path must not silently discard an event while emergency TX owns \
             registers"
        );
        assert!(bridge.doorbell.is_pending());
    }

    #[test]
    fn emergency_release_notifies_the_deferred_worker() {
        let gate = UartRegisterGate::new(BoundedEmergencyTx);
        let stats = SerialStatsAtomic::new();
        let notifications = Cell::new(0);

        assert_eq!(
            route_emergency_bytes(&gate, &stats, b"panic", || {
                notifications.set(notifications.get() + 1);
            }),
            2
        );
        assert_eq!(
            notifications.get(),
            1,
            "releasing the emergency register gate must republish deferred worker progress"
        );
        assert_eq!(stats.snapshot().log_dropped, 3);
        assert!(
            gate.try_enter().is_some(),
            "the release notification must run after the register gate is dropped"
        );
    }
}
