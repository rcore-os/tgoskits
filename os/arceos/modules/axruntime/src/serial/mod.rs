//! UART runtime ownership and task-context data service.
//!
//! Each UART has one CPU-affine maintenance task. Sleepable TTY output and
//! non-blocking per-CPU log records use separate bounded queues; only the IRQ
//! endpoint, maintenance task, and emergency endpoint touch UART registers.

mod control;
mod ingress;
mod log_mailbox;
mod spsc;
mod state;
mod worker;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_driver::serial::SerialDevice;
pub use ax_driver::serial::SerialDeviceInfo;
use ax_lazyinit::OnceLock;
use ax_sync::Mutex;
use ax_task::{AxCpuMask, IrqNotify, TaskInner, WaitQueue};
use axpoll::{IoEvents, PollSet};
pub use rdif_serial::{Config, ConfigError, DataBits, Parity, RxFlag, StopBits};
pub use state::SerialStats;

use self::{
    control::{ControlOp, ControlQueue},
    ingress::TxIngress,
    log_mailbox::{LogMailbox, LogRecordMeta},
    spsc::{Consumer as SpscConsumer, Producer as SpscProducer},
    state::{SerialIrqLatch, SerialStatsAtomic},
    worker::SerialWorker,
};
use crate::{RuntimeError, RuntimeResult, sync::SpinLock};

const NO_ACTIVE_CONSOLE: usize = usize::MAX;
const IRQ_RX_CAPACITY: usize = 16_384;
const SUBSCRIPTION_RX_CAPACITY: usize = 4_096;

static SERIAL_RUNTIMES: OnceLock<Box<[SerialRuntimeHandle]>> = OnceLock::new();
static LOG_MAILBOX: OnceLock<Arc<LogMailbox>> = OnceLock::new();
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
    port: SpinLock<Box<dyn rdif_serial::UartPort>>,
    register_gate: Arc<rdif_serial::UartRegisterGate<dyn rdif_serial::UartEmergencyTx>>,
    ingress: TxIngress,
    log_mailbox: Arc<LogMailbox>,
    rx_subscription: SpinLock<Option<SpscConsumer<RxItem>>>,
    control: ControlQueue,
    bridge: Arc<RuntimeIrqBridge>,
    stats: Arc<SerialStatsAtomic>,
    rx_source: Arc<PollSet>,
    tx_source: Arc<PollSet>,
    rx_progress: WaitQueue,
    tx_progress: WaitQueue,
    tty_output_lock: Mutex<()>,
    log_barriers: AtomicUsize,
    started: AtomicBool,
    irq_handle: OnceLock<ax_hal::irq::IrqHandle>,
}

impl RuntimeShared {
    /// Runs one task-context register transaction with local IRQ delivery
    /// excluded and all cross-CPU aliases serialized by the UART gate.
    fn with_port<R>(&self, access: impl FnOnce(&mut dyn rdif_serial::UartPort) -> R) -> R {
        let mut port = self.port.lock_irqsave();
        let _register_access = loop {
            if let Some(access) = self.register_gate.try_enter() {
                break access;
            }
            core::hint::spin_loop();
        };
        access(&mut **port)
    }

    fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    fn ensure_started(&self) -> RuntimeResult {
        self.started()
            .then_some(())
            .ok_or(RuntimeError::SerialNotStarted)
    }

    fn set_started(&self, started: bool) {
        self.started.store(started, Ordering::Release);
        if !started {
            self.rx_progress.notify_all(true);
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

    fn enable_irq(&self) -> RuntimeResult {
        let Some(handle) = self.irq_handle.get().copied() else {
            return Ok(());
        };
        ax_hal::irq::enable_irq(handle).map_err(|error| {
            warn!(
                "failed to enable serial IRQ for {}: {error:?}",
                self.info.name
            );
            RuntimeError::from(error)
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

    /// Leases the only RX subscription.
    ///
    /// Dropping the subscription returns the consumer to this runtime so a
    /// failed owner initialization does not permanently consume the RX path.
    pub fn take_rx_subscription(&self) -> Option<SerialRxSubscription> {
        let consumer = self.shared.rx_subscription.lock_irqsave().take()?;
        Some(SerialRxSubscription {
            consumer: SpinLock::new(Some(consumer)),
            shared: self.shared.clone(),
        })
    }

    pub fn start(&self, config: Config) -> RuntimeResult {
        self.shared
            .control
            .submit(ControlOp::Start(config), &self.shared.bridge.notify)
    }

    pub fn shutdown(&self) -> RuntimeResult {
        let result = self
            .shared
            .control
            .submit(ControlOp::Shutdown, &self.shared.bridge.notify);
        if result.is_ok() {
            deactivate_console(&self.shared);
        }
        result
    }

    pub fn set_config(&self, config: Config) -> RuntimeResult {
        self.output_barrier()?.set_config(config)
    }

    /// Pauses extraction of new log records until the returned guard drops.
    pub fn output_barrier(&self) -> RuntimeResult<SerialOutputBarrier> {
        self.shared.ensure_started()?;
        Ok(SerialOutputBarrier::new(self.shared.clone()))
    }

    /// Blocks new early-console register access before runtime configuration.
    pub fn begin_console_handoff(&self) -> RuntimeResult {
        ax_hal::console::begin_runtime_handoff()?;
        Ok(())
    }

    /// Starts a console whose low-level path is already in `Preparing`.
    ///
    /// Configuration failures are recoverable because UART `startup()` must
    /// restore its pre-call register state. Failures after successful hardware
    /// configuration fail closed because the former early state is uncertain.
    pub fn start_prepared_console(&self, config: Config) -> RuntimeResult {
        match self.start(config) {
            Ok(()) => Ok(()),
            Err(error @ RuntimeError::SerialConfig(ConfigError::RegisterError)) => {
                ax_hal::console::fail_runtime_handoff_closed();
                Err(error)
            }
            Err(error @ RuntimeError::SerialConfig(_))
            | Err(error @ RuntimeError::SerialControlBusy) => {
                if ax_hal::console::rollback_runtime_handoff().is_err() {
                    ax_hal::console::fail_runtime_handoff_closed();
                }
                Err(error)
            }
            Err(error) => {
                ax_hal::console::fail_runtime_handoff_closed();
                Err(error)
            }
        }
    }

    /// Publishes runtime log routing and completes the platform handoff.
    pub fn commit_console_handoff(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        if ACTIVE_CONSOLE
            .compare_exchange(
                NO_ACTIVE_CONSOLE,
                self.shared.index,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            ax_hal::console::fail_runtime_handoff_closed();
            return Err(RuntimeError::SerialConsoleBusy);
        }
        if let Err(error) = ax_hal::console::commit_runtime_handoff() {
            let _ = ACTIVE_CONSOLE.compare_exchange(
                self.shared.index,
                NO_ACTIVE_CONSOLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            ax_hal::console::fail_runtime_handoff_closed();
            return Err(error.into());
        }
        if !self.shared.log_mailbox.claim(self.shared.index) {
            let _ = ACTIVE_CONSOLE.compare_exchange(
                self.shared.index,
                NO_ACTIVE_CONSOLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            ax_hal::console::fail_runtime_handoff_closed();
            return Err(RuntimeError::SerialConsoleBusy);
        }
        self.shared.bridge.notify.notify();
        Ok(())
    }

    /// Restores early ownership before runtime hardware configuration begins.
    pub fn rollback_console_handoff(&self) -> RuntimeResult {
        ax_hal::console::rollback_runtime_handoff()?;
        Ok(())
    }

    /// Prevents further early access after an ownership failure.
    pub fn fail_console_handoff_closed(&self) {
        ax_hal::console::fail_runtime_handoff_closed();
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
    pub fn try_write(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        self.shared.ensure_started()?;
        let accepted = self
            .shared
            .ingress
            .try_write(bytes, &self.shared.bridge.notify);
        if accepted == 0 {
            Err(RuntimeError::WouldBlock)
        } else {
            Ok(accepted)
        }
    }

    pub fn wait_writable(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.shared
            .tx_progress
            .wait_until(|| self.shared.ingress.write_room() > 0 || !self.shared.started());
        self.shared
            .started()
            .then_some(())
            .ok_or(RuntimeError::SerialNotStarted)
    }

    /// Writes every raw byte, sleeping only when the bounded runtime queue is full.
    pub fn write_all(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        self.write_all_with(bytes, |shared, remaining| {
            shared.ingress.try_write(remaining, &shared.bridge.notify)
        })
    }

    /// Writes every text byte while expanding line feeds to CRLF.
    pub fn write_text_all(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        self.write_all_with(bytes, |shared, remaining| {
            shared
                .ingress
                .try_write_text(remaining, &shared.bridge.notify)
        })
    }

    fn write_all_with(
        &self,
        bytes: &[u8],
        submit: impl Fn(&RuntimeShared, &[u8]) -> usize,
    ) -> RuntimeResult<usize> {
        let mut written = 0;
        while written < bytes.len() {
            self.shared.ensure_started()?;
            let accepted = submit(&self.shared, &bytes[written..]);
            if accepted == 0 {
                self.wait_writable()?;
            } else {
                written += accepted;
            }
        }
        Ok(written)
    }

    pub fn wait_idle(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        SerialOutputBarrier::new(self.shared.clone()).wait_idle()
    }

    pub fn discard_pending(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.shared
            .control
            .submit(ControlOp::DiscardTx, &self.shared.bridge.notify)
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        self.shared.tx_source.clone()
    }
}

/// Sleepable TTY/configuration transaction which excludes new log extraction.
pub struct SerialOutputBarrier {
    shared: Arc<RuntimeShared>,
}

impl SerialOutputBarrier {
    fn new(shared: Arc<RuntimeShared>) -> Self {
        shared.log_barriers.fetch_add(1, Ordering::AcqRel);
        shared.bridge.notify.notify();
        Self { shared }
    }

    /// Waits for queued TTY bytes, the current log record, and UART hardware
    /// to become idle. New log records remain paused after this method returns.
    pub fn wait_idle(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.shared.ingress.begin_drain();
        self.shared.bridge.notify.notify();
        self.shared
            .tx_progress
            .wait_until(|| self.shared.ingress.is_idle() || !self.shared.started());
        if self.shared.ingress.is_idle() {
            Ok(())
        } else {
            Err(RuntimeError::SerialNotStarted)
        }
    }

    /// Applies configuration before allowing worker log extraction to resume.
    pub fn set_config(&self, config: Config) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.shared
            .control
            .submit(ControlOp::SetConfig(config), &self.shared.bridge.notify)
    }
}

impl Drop for SerialOutputBarrier {
    fn drop(&mut self) {
        self.shared.log_barriers.fetch_sub(1, Ordering::AcqRel);
        self.shared.bridge.notify.notify();
    }
}

/// The unique RX consumer for one UART runtime.
pub struct SerialRxSubscription {
    consumer: SpinLock<Option<SpscConsumer<RxItem>>>,
    shared: Arc<RuntimeShared>,
}

impl SerialRxSubscription {
    pub fn drain(&self, out: &mut [RxItem]) -> usize {
        let count = {
            let mut subscription = self.consumer.lock_irqsave();
            // `None` is only observable from `Drop`, which requires exclusive
            // access. Keep the runtime boundary non-panicking if that invariant
            // is changed by a future ownership refactor.
            let Some(consumer) = subscription.as_mut() else {
                return 0;
            };
            consumer.drain(out)
        };
        notify_drained_space(count, || self.shared.bridge.notify.notify());
        count
    }

    /// Blocks until RX data is available or the runtime stops.
    pub fn wait_readable(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.shared.rx_progress.wait_until(|| {
            self.consumer
                .lock_irqsave()
                .as_ref()
                .is_some_and(|consumer| !consumer.is_empty())
                || !self.shared.started()
        });
        self.consumer
            .lock_irqsave()
            .as_ref()
            .is_some_and(|consumer| !consumer.is_empty())
            .then_some(())
            .ok_or(RuntimeError::SerialNotStarted)
    }

    pub fn discard_pending(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.clear_pending();
        let result = self
            .shared
            .control
            .submit(ControlOp::DiscardRx, &self.shared.bridge.notify);
        self.clear_pending();
        result
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        self.shared.rx_source.clone()
    }

    fn clear_pending(&self) {
        if let Some(consumer) = self.consumer.lock_irqsave().as_mut() {
            consumer.clear();
        }
        self.shared.bridge.notify.notify();
    }
}

impl Drop for SerialRxSubscription {
    fn drop(&mut self) {
        let Some(consumer) = self.consumer.get_mut().take() else {
            return;
        };
        let mut available = self.shared.rx_subscription.lock_irqsave();
        debug_assert!(
            available.is_none(),
            "serial runtime cannot have two RX consumers"
        );
        if available.is_none() {
            *available = Some(consumer);
        }
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
    let log_mailbox = LOG_MAILBOX
        .call_once(|| Arc::new(LogMailbox::new(ax_hal::cpu_num().max(1))))
        .clone();
    let mut handles = Vec::new();
    for serial in ax_driver::serial::take_serial_devices() {
        match build_runtime(handles.len(), primary_cpu, serial, log_mailbox.clone()) {
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
    log_mailbox: Arc<LogMailbox>,
) -> RuntimeResult<SerialRuntimeHandle> {
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
    let register_gate: Arc<rdif_serial::UartRegisterGate<dyn rdif_serial::UartEmergencyTx>> =
        Arc::from(register_gate);
    let (irq_rx_producer, irq_rx_consumer) = spsc::channel(IRQ_RX_CAPACITY);
    let (rx_output_producer, rx_output_consumer) = spsc::channel(SUBSCRIPTION_RX_CAPACITY);
    let shared = Arc::new(RuntimeShared {
        index,
        info,
        owner_cpu: primary_cpu,
        polling,
        port: SpinLock::new(port),
        register_gate: register_gate.clone(),
        ingress: TxIngress::new(),
        log_mailbox,
        rx_subscription: SpinLock::new(Some(rx_output_consumer)),
        control: ControlQueue::new(),
        bridge: bridge.clone(),
        stats: stats.clone(),
        rx_source: Arc::new(PollSet::new()),
        tx_source: Arc::new(PollSet::new()),
        rx_progress: WaitQueue::new(),
        tx_progress: WaitQueue::new(),
        tty_output_lock: Mutex::new(()),
        log_barriers: AtomicUsize::new(0),
        started: AtomicBool::new(false),
        irq_handle: OnceLock::new(),
    });

    let worker = SerialWorker::new(shared.clone(), irq_rx_consumer, rx_output_producer);
    let task = TaskInner::new(
        move || worker.run(),
        alloc::format!("serial{index}-maint"),
        ax_task::default_task_stack_size(),
    );
    task.set_cpumask(AxCpuMask::one_shot(primary_cpu));

    if let Some(binding) = shared.info.irq.clone() {
        let irq_id = crate::irq::resolve_binding_irq(binding).map_err(|error| {
            warn!(
                "failed to resolve serial IRQ for {}: {error:?}",
                shared.info.name
            );
            RuntimeError::from(error)
        })?;
        let callback_bridge = bridge;
        let callback_stats = stats;
        let mut callback_rx = RuntimeIrqPublisher {
            producer: irq_rx_producer,
            bridge: callback_bridge.clone(),
            stats: callback_stats.clone(),
        };
        let callback_gate = register_gate;
        let request = serial_irq_request(
            ax_hal::irq::IrqRequest::new(move |_| {
                let Some(_register_access) = callback_gate.try_enter() else {
                    return ax_hal::irq::IrqReturn::Unhandled;
                };
                let Some(report) = irq.handle() else {
                    callback_stats.spurious_irq();
                    return ax_hal::irq::IrqReturn::Unhandled;
                };
                let event = callback_rx.publish(report);
                mask_deferred_irq_rx(&mut *irq, event);
                callback_stats.handled_irq(event);
                callback_bridge.latch.publish(event);
                callback_bridge.notify.notify_irq();
                ax_hal::irq::IrqReturn::Handled
            }),
            primary_cpu,
        );
        let handle = ax_hal::irq::request_irq(irq_id, request).map_err(|error| {
            warn!(
                "failed to register serial IRQ for {}: {error:?}",
                shared.info.name
            );
            RuntimeError::from(error)
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

struct RuntimeIrqPublisher {
    producer: SpscProducer<rdif_serial::RxSample>,
    bridge: Arc<RuntimeIrqBridge>,
    stats: Arc<SerialStatsAtomic>,
}

impl RuntimeIrqPublisher {
    fn publish(&mut self, mut report: rdif_serial::SerialIrqReport) -> rdif_serial::SerialIrqEvent {
        for &sample in report.rx.as_slice() {
            if self.producer.push(sample).is_err() {
                self.stats.add_rx_dropped(1);
                self.bridge.rx_overflow.store(true, Ordering::Release);
                report.event.rx_errors |= rdif_serial::RxErrorFlags::OVERRUN;
                report.event.rearm |= rdif_serial::SerialEventSet::RX;
            }
        }
        report.event
    }
}

fn mask_deferred_irq_rx(irq: &mut dyn rdif_serial::UartIrq, event: rdif_serial::SerialIrqEvent) {
    if event.rearm.intersects(rdif_serial::SerialEventSet::RX) {
        irq.mask(rdif_serial::SerialEventSet::RX);
    }
}

/// Publishes one complete ordinary record without waiting for UART progress.
pub(crate) fn try_publish_record(
    meta: ax_log::RecordMeta,
    args: fmt::Arguments<'_>,
) -> Option<ax_log::PublishStatus> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    let guard = ax_task::sync::PreemptIrqSaveGuard::new();
    // SAFETY: `guard` prevents task migration and local IRQ re-entry for the
    // whole callback; runtime CPU-local state is installed before handoff.
    let outcome = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            let cpu_id = ax_hal::percpu::this_cpu_id_pinned(pin);
            let task_id = ax_task::current_may_uninit().map(|task| task.id().as_u64());
            let timestamp_nanos = ax_hal::time::monotonic_time().as_nanos() as u64;
            let record_meta = match meta.kind() {
                ax_log::RecordKind::Print => LogRecordMeta::print(timestamp_nanos, task_id),
                ax_log::RecordKind::Log => LogRecordMeta::log(timestamp_nanos, task_id),
            };
            runtime
                .shared
                .log_mailbox
                .try_publish(cpu_id, record_meta, args)
        })
    }
    .unwrap_or_else(|_| log_mailbox::PublishOutcome::dropped(0));
    drop(guard);
    runtime
        .shared
        .stats
        .add_log_dropped(outcome.dropped_source_bytes());
    runtime
        .shared
        .stats
        .add_log_dropped_records(outcome.dropped_records());
    if outcome.published() {
        if ax_hal::irq::in_irq_context() {
            runtime.shared.bridge.notify.notify_irq();
        } else {
            runtime.shared.bridge.notify.notify();
        }
    }
    Some(if !outcome.published() {
        ax_log::PublishStatus::Dropped
    } else if outcome.truncated() {
        ax_log::PublishStatus::Truncated
    } else {
        ax_log::PublishStatus::Published
    })
}

/// Formats and attempts one bounded emergency write without the log mailbox.
pub(crate) fn emergency_write(args: fmt::Arguments<'_>) -> Option<usize> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    let Some(_formatting) = EmergencyFormatting::try_enter() else {
        runtime.shared.stats.add_log_dropped_records(1);
        return Some(0);
    };
    let mut buffer = EmergencyBuffer::new();
    if buffer.write_fmt(args).is_err() {
        runtime.shared.stats.add_log_dropped_records(1);
        return Some(0);
    }
    let Some(register_access) = runtime.shared.register_gate.try_enter() else {
        runtime.shared.stats.add_log_dropped(buffer.source_len);
        runtime.shared.stats.add_log_dropped_records(1);
        return Some(0);
    };
    let written = register_access.try_write(buffer.bytes());
    runtime.shared.stats.add_log_dropped(
        buffer
            .source_len
            .saturating_sub(buffer.accepted_source_bytes(written)),
    );
    if written < buffer.bytes().len() || buffer.truncated {
        runtime.shared.stats.add_log_dropped_records(1);
    }
    Some(written)
}

const EMERGENCY_OUTPUT_BYTES: usize = 1024;
static EMERGENCY_FORMATTING: AtomicBool = AtomicBool::new(false);

struct EmergencyFormatting;

impl EmergencyFormatting {
    fn try_enter() -> Option<Self> {
        EMERGENCY_FORMATTING
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for EmergencyFormatting {
    fn drop(&mut self) {
        EMERGENCY_FORMATTING.store(false, Ordering::Release);
    }
}

struct EmergencyBuffer {
    bytes: [u8; EMERGENCY_OUTPUT_BYTES],
    len: usize,
    source_len: usize,
    truncated: bool,
}

impl EmergencyBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; EMERGENCY_OUTPUT_BYTES],
            len: 0,
            source_len: 0,
            truncated: false,
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn accepted_source_bytes(&self, written: usize) -> usize {
        let written = written.min(self.len);
        written
            .saturating_sub(
                self.bytes[..written]
                    .iter()
                    .filter(|&&byte| byte == b'\n')
                    .count(),
            )
            .min(self.source_len)
    }
}

impl Write for EmergencyBuffer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.source_len = self.source_len.saturating_add(text.len());
        if self.truncated {
            return Ok(());
        }
        for character in text.chars() {
            let required = character.len_utf8() + usize::from(character == '\n');
            if self.len + required > self.bytes.len() {
                self.truncated = true;
                break;
            }
            if character == '\n' {
                self.bytes[self.len] = b'\r';
                self.len += 1;
            }
            let end = self.len + character.len_utf8();
            character.encode_utf8(&mut self.bytes[self.len..end]);
            self.len = end;
        }
        Ok(())
    }
}

fn deactivate_console(shared: &RuntimeShared) {
    if ACTIVE_CONSOLE
        .compare_exchange(
            shared.index,
            NO_ACTIVE_CONSOLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        shared.log_mailbox.release(shared.index);
        shared.bridge.notify.notify();
    }
}

/// Writes text through the active runtime console, if one has claimed output.
pub fn write_active_console_text(bytes: &[u8]) -> Option<RuntimeResult<usize>> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    let _output = runtime.shared.tty_output_lock.lock();
    Some(runtime.tx_sender().write_text_all(bytes))
}

/// Formats through the sleepable TTY path while serializing all fragments.
pub fn write_active_console_fmt(args: fmt::Arguments<'_>) -> Option<fmt::Result> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    let _output = runtime.shared.tty_output_lock.lock();
    let mut writer = ActiveConsoleWriter {
        sender: runtime.tx_sender(),
    };
    Some(writer.write_fmt(args))
}

/// Runs the real-SMP mailbox ownership probe used by the kernel axtest suite.
#[cfg(axtest)]
#[doc(hidden)]
pub fn smp_log_mailbox_contract_holds() -> bool {
    const TEST_CPUS: usize = 4;
    const RECORDS_PER_CPU: usize = 16;
    const TEST_OWNER: usize = usize::MAX - 1;
    const TTY_CAPACITY: usize = 16 * 256;

    if ax_hal::cpu_num() < TEST_CPUS {
        return false;
    }

    let mailbox = Arc::new(LogMailbox::new(TEST_CPUS));
    if !mailbox.claim(TEST_OWNER) {
        return false;
    }

    let ready = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(AtomicBool::new(false));
    let passed = Arc::new(AtomicBool::new(true));
    let mut tasks = Vec::with_capacity(TEST_CPUS);
    for cpu_id in 0..TEST_CPUS {
        let mailbox = mailbox.clone();
        let ready = ready.clone();
        let start = start.clone();
        let passed = passed.clone();
        let task = TaskInner::new(
            move || {
                if ax_hal::percpu::this_cpu_id() != cpu_id {
                    passed.store(false, Ordering::Release);
                }
                ready.fetch_add(1, Ordering::Release);
                while !start.load(Ordering::Acquire) {
                    ax_task::yield_now();
                }
                for local_sequence in 0..RECORDS_PER_CPU {
                    let checksum = smp_log_test_checksum(cpu_id, local_sequence);
                    let outcome = mailbox.try_publish(
                        cpu_id,
                        LogRecordMeta::print(local_sequence as u64, None),
                        format_args!(
                            "mailbox-smp cpu={cpu_id} seq={local_sequence} \
                             checksum={checksum:08x}\n"
                        ),
                    );
                    if !outcome.published()
                        || outcome.truncated()
                        || outcome.dropped_records() != 0
                        || outcome.dropped_source_bytes() != 0
                    {
                        passed.store(false, Ordering::Release);
                    }
                }
            },
            alloc::format!("log-mailbox-producer-{cpu_id}"),
            ax_task::default_task_stack_size(),
        );
        tasks.push(ax_task::spawn_task_with(task, |task| {
            task.set_cpumask(AxCpuMask::one_shot(cpu_id));
        }));
    }

    while ready.load(Ordering::Acquire) != TEST_CPUS {
        ax_task::yield_now();
    }
    start.store(true, Ordering::Release);
    for task in tasks {
        if task.join() != 0 {
            passed.store(false, Ordering::Release);
        }
    }

    let ingress = TxIngress::new();
    ingress.start_accepting();
    let tty_payload = [b'T'; TTY_CAPACITY];
    if ingress.try_write(&tty_payload, &IrqNotify::new()) != TTY_CAPACITY {
        passed.store(false, Ordering::Release);
    }

    let mut reader = mailbox.reader();
    for index in 0..TEST_CPUS * RECORDS_PER_CPU {
        let expected_cpu = index % TEST_CPUS;
        let expected_sequence = index / TEST_CPUS;
        let checksum = smp_log_test_checksum(expected_cpu, expected_sequence);
        let expected = alloc::format!(
            "mailbox-smp cpu={expected_cpu} seq={expected_sequence} checksum={checksum:08x}\r\n"
        );
        let Some(consumed) = reader.take(TEST_OWNER) else {
            return false;
        };
        if consumed.sequence_gap != 0
            || consumed.record.cpu_id() != expected_cpu
            || consumed.record.sequence() != expected_sequence as u64
            || consumed.record.bytes() != expected.as_bytes()
        {
            passed.store(false, Ordering::Release);
        }
    }
    if reader.take(TEST_OWNER).is_some() {
        passed.store(false, Ordering::Release);
    }
    mailbox.release(TEST_OWNER);
    passed.load(Ordering::Acquire)
}

#[cfg(axtest)]
const fn smp_log_test_checksum(cpu_id: usize, local_sequence: usize) -> u32 {
    (cpu_id as u32).wrapping_mul(0x9e37_79b9)
        ^ (local_sequence as u32).wrapping_mul(0x85eb_ca6b)
        ^ 0xa5a5_5a5a
}

struct ActiveConsoleWriter {
    sender: SerialTxSender,
}

impl Write for ActiveConsoleWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.sender
            .write_text_all(text.as_bytes())
            .map(|_| ())
            .map_err(|_| fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingIrq {
        masked: rdif_serial::SerialEventSet,
    }

    impl rdif_serial::UartIrq for RecordingIrq {
        fn mask(&mut self, sources: rdif_serial::SerialEventSet) {
            self.masked |= sources;
        }

        fn handle(&mut self) -> Option<rdif_serial::SerialIrqReport> {
            None
        }
    }

    #[test]
    fn irq_report_drops_only_after_the_preallocated_ring_is_full() {
        let bridge = Arc::new(RuntimeIrqBridge::new());
        let stats = Arc::new(SerialStatsAtomic::new());
        let (producer, mut consumer) = spsc::channel(2);
        let mut publisher = RuntimeIrqPublisher {
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
        let mut batch = rdif_serial::IrqRxBatch::new();
        for sample in samples {
            batch.try_push(sample).unwrap();
        }
        let event = publisher.publish(rdif_serial::SerialIrqReport::new(
            rdif_serial::SerialIrqEvent::default(),
            batch,
        ));

        assert_eq!(consumer.pop().and_then(|sample| sample.byte), Some(1));
        assert_eq!(consumer.pop().and_then(|sample| sample.byte), Some(2));
        assert!(consumer.pop().is_none());
        assert_eq!(stats.snapshot().rx_dropped, 1);
        assert!(bridge.rx_overflow.load(Ordering::Acquire));
        assert!(event.rx_errors.contains(rdif_serial::RxErrorFlags::OVERRUN));
        assert!(event.rearm.contains(rdif_serial::SerialEventSet::RX));
    }

    #[test]
    fn deferred_rx_masks_only_the_uart_source() {
        let mut irq = RecordingIrq {
            masked: rdif_serial::SerialEventSet::empty(),
        };
        mask_deferred_irq_rx(
            &mut irq,
            rdif_serial::SerialIrqEvent {
                rearm: rdif_serial::SerialEventSet::RX | rdif_serial::SerialEventSet::TX_SPACE,
                ..rdif_serial::SerialIrqEvent::default()
            },
        );

        assert_eq!(irq.masked, rdif_serial::SerialEventSet::RX);
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
    fn absent_runtime_console_preserves_early_publication_fallback() {
        ACTIVE_CONSOLE.store(NO_ACTIVE_CONSOLE, Ordering::Release);
        assert_eq!(
            try_publish_record(ax_log::RecordMeta::print(), format_args!("fallback")),
            None
        );
    }
}
