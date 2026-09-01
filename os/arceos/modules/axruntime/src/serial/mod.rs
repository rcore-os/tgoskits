//! UART runtime ownership and task-context data service.
//!
//! Each UART has one CPU-affine maintenance task. Sleepable TTY output and
//! non-blocking per-CPU log records use separate bounded queues; only the IRQ
//! endpoint, maintenance task, and emergency endpoint touch UART registers.

mod control;
mod ingress;
mod log_mailbox;
pub(crate) mod spsc;
mod state;
mod worker;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

use ax_driver::serial::SerialDevice;
pub use ax_driver::serial::SerialDeviceInfo;
use ax_lazyinit::OnceLock;
use ax_sync::Mutex;
use ax_task::{AxCpuMask, IrqNotify, TaskInner, WaitQueue};
use axpoll::{IoEvents, PollSet};
pub use rdif_serial::{Config, ConfigError, DataBits, Parity, RxFlag, StopBits};

pub(crate) use self::log_mailbox::{LogRecord, LogRecordKind};
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
// A subscriber can be unable to run while all secondary CPUs publish their
// startup records. Keep enough whole-record slots for the bounded SMP burst so
// activating a console owner does not immediately lose diagnostics.
const LOG_SUBSCRIPTION_CAPACITY: usize = 128;

static SERIAL_RUNTIMES: OnceLock<Box<[SerialRuntimeHandle]>> = OnceLock::new();
static LOG_MAILBOX: OnceLock<Arc<LogMailbox>> = OnceLock::new();
static ACTIVE_CONSOLE: AtomicUsize = AtomicUsize::new(NO_ACTIVE_CONSOLE);

const RUNTIME_DORMANT: u8 = 0;
const RUNTIME_STARTED: u8 = 1;
const RUNTIME_FAILED_CLOSED: u8 = 2;

struct RuntimeLifecycle(AtomicU8);

impl RuntimeLifecycle {
    const fn new() -> Self {
        Self(AtomicU8::new(RUNTIME_DORMANT))
    }

    fn started(&self) -> bool {
        self.0.load(Ordering::Acquire) == RUNTIME_STARTED
    }

    fn ensure_available(&self) -> RuntimeResult {
        (self.0.load(Ordering::Acquire) != RUNTIME_FAILED_CLOSED)
            .then_some(())
            .ok_or(RuntimeError::ConsoleFailedClosed)
    }

    fn ensure_started(&self) -> RuntimeResult {
        match self.0.load(Ordering::Acquire) {
            RUNTIME_STARTED => Ok(()),
            RUNTIME_FAILED_CLOSED => Err(RuntimeError::ConsoleFailedClosed),
            RUNTIME_DORMANT => Err(RuntimeError::SerialNotStarted),
            _ => unreachable!(),
        }
    }

    fn set_started(&self, started: bool) {
        let next = if started {
            RUNTIME_STARTED
        } else {
            RUNTIME_DORMANT
        };
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state != RUNTIME_FAILED_CLOSED).then_some(next)
            });
    }

    fn fail_closed(&self) {
        self.0.store(RUNTIME_FAILED_CLOSED, Ordering::Release);
    }
}

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
    log_subscription: SpinLock<Option<SpscConsumer<LogRecord>>>,
    log_subscription_gate: SpinLock<()>,
    log_subscription_active: AtomicBool,
    log_subscription_dropped_records: AtomicUsize,
    log_subscription_dropped_bytes: AtomicUsize,
    control: ControlQueue,
    bridge: Arc<RuntimeIrqBridge>,
    stats: Arc<SerialStatsAtomic>,
    rx_source: Arc<PollSet>,
    tx_source: Arc<PollSet>,
    rx_progress: WaitQueue,
    console_progress: WaitQueue,
    tx_progress: WaitQueue,
    tty_output_lock: Mutex<()>,
    log_barriers: AtomicUsize,
    lifecycle: RuntimeLifecycle,
    irq_handle: OnceLock<ax_hal::irq::IrqHandle>,
}

impl RuntimeShared {
    /// Runs one task-context register transaction with local IRQ delivery
    /// excluded and all cross-CPU aliases serialized by the UART gate.
    fn with_port<R>(&self, access: impl FnOnce(&mut dyn rdif_serial::UartPort) -> R) -> Option<R> {
        let mut port = self.port.lock_irqsave();
        let _register_access = loop {
            if self.register_gate.emergency_active() {
                return None;
            }
            if let Some(access) = self.register_gate.try_enter() {
                break access;
            }
            core::hint::spin_loop();
        };
        Some(access(&mut **port))
    }

    fn started(&self) -> bool {
        self.lifecycle.started()
    }

    fn ensure_started(&self) -> RuntimeResult {
        self.lifecycle.ensure_started()
    }

    fn set_started(&self, started: bool) {
        self.lifecycle.set_started(started);
        if !started {
            self.rx_progress.notify_all(true);
            self.console_progress.notify_all(true);
            self.tx_progress.notify_all(true);
        }
    }

    fn fail_closed(&self) {
        self.lifecycle.fail_closed();
        self.disable_irq();
        // `FailedClosed` is not merely an API state. Terminally claim the
        // register gate so a final in-flight worker or IRQ transaction cannot
        // hand the UART back to a normal endpoint afterward. The lifecycle
        // publication and disabled IRQ prevent new contenders; an existing
        // bounded register transaction is allowed to finish.
        while !self.register_gate.emergency_active() {
            if let Some(access) = self.register_gate.try_begin_emergency() {
                drop(access);
                break;
            }
            core::hint::spin_loop();
        }
        self.ingress.stop_and_discard();
        self.rx_progress.notify_all(true);
        self.console_progress.notify_all(true);
        self.tx_progress.notify_all(true);
    }

    fn publish_tx_space(&self) {
        self.tx_progress.notify_all(true);
        // SAFETY: the maintenance task publishes queue space before waking
        // task-context poll waiters.
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

    /// Leases the only RX subscription.
    ///
    /// Dropping the subscription returns the consumer to this runtime so a
    /// failed owner initialization does not permanently consume the RX path.
    pub fn take_rx_subscription(&self) -> Option<SerialRxSubscription> {
        self.shared.lifecycle.ensure_available().ok()?;
        let consumer = self.shared.rx_subscription.lock_irqsave().take()?;
        Some(SerialRxSubscription {
            consumer: SpinLock::new(Some(consumer)),
            shared: self.shared.clone(),
        })
    }

    pub(crate) fn take_log_subscription(&self) -> Option<SerialLogSubscription> {
        self.shared.lifecycle.ensure_available().ok()?;
        let _route = self.shared.log_subscription_gate.lock_irqsave();
        if self.shared.log_subscription_active.load(Ordering::Acquire) {
            return None;
        }
        let mut available = self.shared.log_subscription.lock_irqsave();
        let mut consumer = available.take()?;
        consumer.clear();
        self.shared
            .log_subscription_dropped_records
            .store(0, Ordering::Release);
        self.shared
            .log_subscription_dropped_bytes
            .store(0, Ordering::Release);
        self.shared
            .log_subscription_active
            .store(true, Ordering::Release);
        Some(SerialLogSubscription {
            consumer: SpinLock::new(Some(consumer)),
            shared: self.shared.clone(),
        })
    }

    /// Returns a cloneable task-context output capability for this UART.
    ///
    /// This per-port API is used by operating systems that expose non-console
    /// serial devices. Physical-console consumers should use
    /// [`crate::console::output`] so raw-HAL fallback and failed-closed state
    /// remain hidden behind the console boundary.
    pub fn task_output(&self) -> SerialTaskOutput {
        SerialTaskOutput {
            shared: self.shared.clone(),
        }
    }

    pub fn start(&self, config: Config) -> RuntimeResult {
        self.shared.lifecycle.ensure_available()?;
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
    pub(crate) fn output_barrier(&self) -> RuntimeResult<SerialOutputBarrier> {
        self.shared.ensure_started()?;
        Ok(SerialOutputBarrier::new(self.shared.clone()))
    }

    /// Blocks new early-console register access before runtime configuration.
    pub(crate) fn begin_console_handoff(&self) -> RuntimeResult {
        ax_hal::console::begin_runtime_handoff()?;
        Ok(())
    }

    /// Adopts the already-running firmware console while the platform path is
    /// in `Preparing`.
    ///
    /// The worker preserves the firmware line/FIFO configuration and only
    /// masks device-local sources before enabling its registered IRQ action.
    /// The console coordinator owns the surrounding handoff transaction and
    /// closes the early path if this operation fails.
    pub(crate) fn adopt_prepared_console(&self) -> RuntimeResult {
        self.shared
            .control
            .submit(ControlOp::AdoptFirmwareConsole, &self.shared.bridge.notify)
    }

    /// Permanently rejects task, IRQ-consumer, and per-port use after a
    /// selected console handoff becomes untrustworthy.
    pub(crate) fn fail_console_closed(&self) {
        self.shared.fail_closed();
    }

    /// Publishes runtime log routing and completes the platform handoff.
    pub(crate) fn commit_console_handoff(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        // Reserve log routing before publishing either console-owner state.
        // Once the platform transition is committed there is no safe early
        // owner to roll back to, so every remaining operation must be
        // infallible.
        if !self.shared.log_mailbox.claim(self.shared.index) {
            let _ = self.shutdown();
            return Err(RuntimeError::SerialConsoleBusy);
        }
        if ACTIVE_CONSOLE
            .compare_exchange(
                NO_ACTIVE_CONSOLE,
                self.shared.index,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            self.shared.log_mailbox.release(self.shared.index);
            let _ = self.shutdown();
            return Err(RuntimeError::SerialConsoleBusy);
        }
        if let Err(error) = ax_hal::console::commit_runtime_handoff() {
            let _ = ACTIVE_CONSOLE.compare_exchange(
                self.shared.index,
                NO_ACTIVE_CONSOLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            self.shared.log_mailbox.release(self.shared.index);
            let _ = self.shutdown();
            return Err(error.into());
        }
        self.shared.bridge.notify.notify();
        Ok(())
    }
}

/// Cloneable, bounded MPSC submission façade. It never accesses UART registers.
#[derive(Clone)]
pub(crate) struct SerialTxSender {
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
}

/// Sleepable TTY/configuration transaction which excludes new log extraction.
pub(crate) struct SerialOutputBarrier {
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
        self.shared.control.submit_drain(&self.shared.bridge.notify)
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

/// Internal complete-record consumer re-exported through `ax_runtime::console`.
pub(crate) struct SerialLogSubscription {
    consumer: SpinLock<Option<SpscConsumer<LogRecord>>>,
    shared: Arc<RuntimeShared>,
}

impl SerialLogSubscription {
    pub(crate) fn try_read(&self) -> Option<LogRecord> {
        self.consumer.lock_irqsave().as_mut()?.pop()
    }

    pub(crate) fn dropped(&self) -> (usize, usize) {
        (
            self.shared
                .log_subscription_dropped_records
                .swap(0, Ordering::AcqRel),
            self.shared
                .log_subscription_dropped_bytes
                .swap(0, Ordering::AcqRel),
        )
    }

    pub(crate) fn wait_readable(&self) -> RuntimeResult {
        self.shared.ensure_started()?;
        self.shared.console_progress.wait_until(|| {
            self.has_pending()
                || !self.shared.log_subscription_active.load(Ordering::Acquire)
                || !self.shared.started()
        });
        self.has_pending()
            .then_some(())
            .ok_or(RuntimeError::SerialNotStarted)
    }

    pub(crate) fn has_pending(&self) -> bool {
        self.shared
            .log_subscription_dropped_records
            .load(Ordering::Acquire)
            != 0
            || self
                .consumer
                .lock_irqsave()
                .as_ref()
                .is_some_and(|consumer| !consumer.is_empty())
    }
}

impl Drop for SerialLogSubscription {
    fn drop(&mut self) {
        let _route = self.shared.log_subscription_gate.lock_irqsave();
        self.shared
            .log_subscription_active
            .store(false, Ordering::Release);
        let Some(mut consumer) = self.consumer.get_mut().take() else {
            return;
        };
        consumer.clear();
        let mut available = self.shared.log_subscription.lock_irqsave();
        debug_assert!(
            available.is_none(),
            "serial runtime cannot have two log consumers"
        );
        if available.is_none() {
            *available = Some(consumer);
        }
        self.shared.console_progress.notify_all(true);
        self.shared.bridge.notify.notify();
    }
}

/// Cloneable task-context output capability for one runtime UART.
#[derive(Clone)]
pub struct SerialTaskOutput {
    shared: Arc<RuntimeShared>,
}

impl SerialTaskOutput {
    pub fn try_write(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        let Some(_output) = self.shared.tty_output_lock.try_lock() else {
            return Err(RuntimeError::WouldBlock);
        };
        SerialTxSender {
            shared: self.shared.clone(),
        }
        .try_write(bytes)
    }

    pub fn write_all(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        let _output = self.shared.tty_output_lock.lock();
        SerialTxSender {
            shared: self.shared.clone(),
        }
        .write_all(bytes)
    }

    pub fn write_text_all(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        let _output = self.shared.tty_output_lock.lock();
        SerialTxSender {
            shared: self.shared.clone(),
        }
        .write_text_all(bytes)
    }

    pub fn write_fmt(&self, args: fmt::Arguments<'_>) -> fmt::Result {
        let _output = self.shared.tty_output_lock.lock();
        let mut writer = ActiveConsoleWriter {
            sender: SerialTxSender {
                shared: self.shared.clone(),
            },
        };
        writer.write_fmt(args)
    }

    pub fn wait_idle(&self) -> RuntimeResult {
        let _output = self.shared.tty_output_lock.lock();
        SerialOutputBarrier::new(self.shared.clone()).wait_idle()
    }

    pub fn discard_pending(&self) -> RuntimeResult {
        let _output = self.shared.tty_output_lock.lock();
        self.shared.ensure_started()?;
        self.shared
            .control
            .submit(ControlOp::DiscardTx, &self.shared.bridge.notify)
    }

    pub fn reconfigure(
        &self,
        config: Option<Config>,
        drain: bool,
        publish: impl FnOnce(),
    ) -> RuntimeResult {
        let _output = self.shared.tty_output_lock.lock();
        let barrier = SerialOutputBarrier::new(self.shared.clone());
        if drain {
            barrier.wait_idle()?;
        }
        if let Some(config) = config {
            barrier.set_config(config)?;
        }
        publish();
        Ok(())
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        self.shared.tx_source.clone()
    }
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

    pub(crate) fn wait_console_event(&self, logs: &SerialLogSubscription) -> RuntimeResult {
        if !Arc::ptr_eq(&self.shared, &logs.shared) {
            return Err(RuntimeError::OperationNotSupported);
        }
        self.shared.ensure_started()?;
        self.shared
            .console_progress
            .wait_until(|| self.has_pending() || logs.has_pending() || !self.shared.started());
        (self.has_pending() || logs.has_pending())
            .then_some(())
            .ok_or(RuntimeError::SerialNotStarted)
    }

    fn has_pending(&self) -> bool {
        self.consumer
            .lock_irqsave()
            .as_ref()
            .is_some_and(|consumer| !consumer.is_empty())
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

pub(crate) fn active_console() -> Option<&'static SerialRuntimeHandle> {
    runtimes().get(ACTIVE_CONSOLE.load(Ordering::Acquire))
}

pub(crate) fn init(primary_cpu: usize) {
    let log_mailbox = LOG_MAILBOX
        .call_once(|| Arc::new(LogMailbox::new(ax_hal::cpu_num().max(1))))
        .clone();
    // `rust_main` initializes the primary scheduler and IPI/IRQ framework
    // before serial discovery, so task-context doorbells are safe on this CPU.
    log_mailbox.mark_wake_ready(primary_cpu);
    let mut handles = Vec::new();
    for serial in ax_driver::serial::take_serial_devices() {
        match build_runtime(handles.len(), primary_cpu, serial, log_mailbox.clone()) {
            Ok(handle) => handles.push(handle),
            Err(err) => warn!("failed to initialize serial runtime: {err:?}"),
        }
    }
    SERIAL_RUNTIMES.call_once(|| handles.into_boxed_slice());
}

#[cfg(feature = "smp")]
pub(crate) fn mark_log_wake_ready(cpu_id: usize) {
    if let Some(log_mailbox) = LOG_MAILBOX.get() {
        log_mailbox.mark_wake_ready(cpu_id);
    }
}

fn build_runtime(
    index: usize,
    primary_cpu: usize,
    serial: SerialDevice,
    log_mailbox: Arc<LogMailbox>,
) -> RuntimeResult<SerialRuntimeHandle> {
    let SerialDevice {
        info,
        port,
        mut irq,
        register_gate,
    } = serial;
    let polling = info.irq.is_none();
    let bridge = Arc::new(RuntimeIrqBridge::new());
    let stats = Arc::new(SerialStatsAtomic::new());
    let register_gate: Arc<rdif_serial::UartRegisterGate<dyn rdif_serial::UartEmergencyTx>> =
        Arc::from(register_gate);
    let (irq_rx_producer, irq_rx_consumer) = spsc::channel(IRQ_RX_CAPACITY);
    let (rx_output_producer, rx_output_consumer) = spsc::channel(SUBSCRIPTION_RX_CAPACITY);
    let (log_subscription_producer, log_subscription_consumer) =
        spsc::channel(LOG_SUBSCRIPTION_CAPACITY);
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
        log_subscription: SpinLock::new(Some(log_subscription_consumer)),
        log_subscription_gate: SpinLock::new(()),
        log_subscription_active: AtomicBool::new(false),
        log_subscription_dropped_records: AtomicUsize::new(0),
        log_subscription_dropped_bytes: AtomicUsize::new(0),
        control: ControlQueue::new(),
        bridge: bridge.clone(),
        stats: stats.clone(),
        rx_source: Arc::new(PollSet::new()),
        tx_source: Arc::new(PollSet::new()),
        rx_progress: WaitQueue::new(),
        console_progress: WaitQueue::new(),
        tx_progress: WaitQueue::new(),
        tty_output_lock: Mutex::new(()),
        log_barriers: AtomicUsize::new(0),
        lifecycle: RuntimeLifecycle::new(),
        irq_handle: OnceLock::new(),
    });

    let worker = SerialWorker::new(
        shared.clone(),
        irq_rx_consumer,
        rx_output_producer,
        log_subscription_producer,
    );
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
        // Preserve the driver's bounded-IRQ decision. A fully drained UART
        // must remain armed while the owner transports its samples; masking a
        // small FIFO until task context runs can overflow at line rate.
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
    let (outcome, log_wake_ready) = unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            let cpu_id = ax_hal::percpu::this_cpu_id_pinned(pin);
            let current = ax_task::current_may_uninit();
            let task_id = current.as_ref().map(|task| task.id().as_u64());
            let timestamp_nanos = ax_hal::time::monotonic_time().as_nanos() as u64;
            let record_meta = match meta.kind() {
                ax_log::RecordKind::Print => LogRecordMeta::print(timestamp_nanos, task_id),
                ax_log::RecordKind::Log => LogRecordMeta::log(timestamp_nanos, task_id),
            };
            (
                runtime
                    .shared
                    .log_mailbox
                    .try_publish(cpu_id, record_meta, args),
                runtime.shared.log_mailbox.wake_ready(cpu_id),
            )
        })
    }
    .unwrap_or_else(|_| (log_mailbox::PublishOutcome::dropped(0), false));
    drop(guard);
    runtime
        .shared
        .stats
        .add_log_dropped(outcome.dropped_source_bytes());
    runtime
        .shared
        .stats
        .add_log_dropped_records(outcome.dropped_records());
    match record_wake_context(
        outcome.published(),
        ax_hal::irq::in_irq_context(),
        log_wake_ready,
    ) {
        RecordWakeContext::Interrupt => {
            runtime.shared.bridge.notify.notify_irq();
        }
        RecordWakeContext::Task => {
            runtime.shared.bridge.notify.notify();
        }
        RecordWakeContext::None => {}
    }
    Some(if !outcome.published() {
        ax_log::PublishStatus::Dropped
    } else if outcome.truncated() {
        ax_log::PublishStatus::Truncated
    } else {
        ax_log::PublishStatus::Published
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordWakeContext {
    None,
    Interrupt,
    Task,
}

const fn record_wake_context(
    published: bool,
    in_irq_context: bool,
    log_wake_ready: bool,
) -> RecordWakeContext {
    if !published || !log_wake_ready {
        RecordWakeContext::None
    } else if in_irq_context {
        RecordWakeContext::Interrupt
    } else {
        RecordWakeContext::Task
    }
}

/// Synchronously streams one emergency record without the log mailbox.
pub(crate) fn emergency_write(args: fmt::Arguments<'_>) -> Option<usize> {
    let index = ACTIVE_CONSOLE.load(Ordering::Acquire);
    let runtime = runtimes().get(index)?;
    let Some(_formatting) = EmergencyFormatting::try_enter() else {
        runtime.shared.stats.add_log_dropped_records(1);
        return Some(0);
    };
    let Some(register_access) = claim_emergency_registers(&runtime.shared.register_gate) else {
        runtime.shared.stats.add_log_dropped_records(1);
        return Some(0);
    };
    let mut writer = EmergencyWriter::new(register_access);
    if writer.write_fmt(args).is_err() {
        runtime.shared.stats.add_log_dropped_records(1);
    }
    Some(writer.source_written)
}

const EMERGENCY_CLAIM_ATTEMPTS: usize = 4096;
static EMERGENCY_FORMATTING: AtomicBool = AtomicBool::new(false);

fn claim_emergency_registers(
    gate: &rdif_serial::UartRegisterGate<dyn rdif_serial::UartEmergencyTx>,
) -> Option<rdif_serial::UartEmergencyAccess<'_, dyn rdif_serial::UartEmergencyTx>> {
    for _ in 0..EMERGENCY_CLAIM_ATTEMPTS {
        if let Some(access) = gate.try_begin_emergency() {
            return Some(access);
        }
        core::hint::spin_loop();
    }
    None
}

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

struct EmergencyWriter<'a, E: rdif_serial::UartEmergencyTx + ?Sized> {
    access: rdif_serial::UartEmergencyAccess<'a, E>,
    source_written: usize,
}

impl<'a, E: rdif_serial::UartEmergencyTx + ?Sized> EmergencyWriter<'a, E> {
    const fn new(access: rdif_serial::UartEmergencyAccess<'a, E>) -> Self {
        Self {
            access,
            source_written: 0,
        }
    }

    fn write_all_blocking(&self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let written = self.access.try_write(bytes).min(bytes.len());
            if written == 0 {
                core::hint::spin_loop();
            } else {
                bytes = &bytes[written..];
            }
        }
    }
}

impl<E: rdif_serial::UartEmergencyTx + ?Sized> Write for EmergencyWriter<'_, E> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut remaining = text.as_bytes();
        while let Some(newline) = remaining.iter().position(|&byte| byte == b'\n') {
            self.write_all_blocking(&remaining[..newline]);
            self.write_all_blocking(b"\r\n");
            remaining = &remaining[newline + 1..];
        }
        self.write_all_blocking(remaining);
        self.source_written = self.source_written.saturating_add(text.len());
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

    struct ChunkedEmergencyTx(&'static AtomicUsize);

    impl rdif_serial::UartEmergencyTx for ChunkedEmergencyTx {
        unsafe fn mask_interrupts_unlocked(&self) {}

        unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize {
            let written = bytes.len().min(7);
            self.0.fetch_add(written, Ordering::Relaxed);
            written
        }
    }

    struct RecordingIrq {
        masked: rdif_serial::SerialEventSet,
    }

    #[test]
    fn failed_closed_runtime_cannot_return_to_dormant_or_started() {
        let lifecycle = RuntimeLifecycle::new();

        assert_eq!(
            lifecycle.ensure_started(),
            Err(RuntimeError::SerialNotStarted)
        );
        lifecycle.set_started(true);
        assert!(lifecycle.ensure_started().is_ok());

        lifecycle.fail_closed();
        assert_eq!(
            lifecycle.ensure_available(),
            Err(RuntimeError::ConsoleFailedClosed)
        );
        assert_eq!(
            lifecycle.ensure_started(),
            Err(RuntimeError::ConsoleFailedClosed)
        );

        lifecycle.set_started(false);
        lifecycle.set_started(true);
        assert_eq!(
            lifecycle.ensure_started(),
            Err(RuntimeError::ConsoleFailedClosed)
        );
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
    fn emergency_writer_streams_a_record_larger_than_the_former_buffer() {
        static HARDWARE_BYTES: AtomicUsize = AtomicUsize::new(0);

        HARDWARE_BYTES.store(0, Ordering::Relaxed);
        let gate = rdif_serial::UartRegisterGate::new(ChunkedEmergencyTx(&HARDWARE_BYTES));
        let access = gate.try_begin_emergency().expect("emergency takeover");
        let mut writer = EmergencyWriter::new(access);
        let payload = "x".repeat(2_048);

        writer.write_str(&payload).unwrap();
        writer.write_str("\nBACKTRACE_END").unwrap();

        assert_eq!(writer.source_written, payload.len() + 14);
        assert_eq!(HARDWARE_BYTES.load(Ordering::Relaxed), payload.len() + 15);
        assert!(gate.try_enter().is_none());
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
    fn fully_drained_rx_irq_keeps_hardware_source_armed() {
        let bridge = Arc::new(RuntimeIrqBridge::new());
        let stats = Arc::new(SerialStatsAtomic::new());
        let (producer, mut consumer) = spsc::channel(2);
        let mut publisher = RuntimeIrqPublisher {
            producer,
            bridge,
            stats,
        };
        let mut batch = rdif_serial::IrqRxBatch::new();
        batch
            .try_push(rdif_serial::RxSample {
                byte: Some(b'x'),
                ..rdif_serial::RxSample::default()
            })
            .unwrap();

        let event = publisher.publish(rdif_serial::SerialIrqReport::new(
            rdif_serial::SerialIrqEvent {
                events: rdif_serial::SerialEventSet::RX_DATA,
                ..rdif_serial::SerialIrqEvent::default()
            },
            batch,
        ));

        assert_eq!(consumer.pop().and_then(|sample| sample.byte), Some(b'x'));
        assert!(
            !event.rearm.contains(rdif_serial::SerialEventSet::RX),
            "a drained IRQ must not leave a small UART FIFO masked until the owner task runs"
        );
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

    #[test]
    fn early_secondary_log_does_not_wake_before_log_wake_ready() {
        assert_eq!(
            record_wake_context(true, false, false),
            RecordWakeContext::None
        );
        assert_eq!(
            record_wake_context(true, false, true),
            RecordWakeContext::Task
        );
        assert_eq!(
            record_wake_context(true, true, false),
            RecordWakeContext::None
        );
        assert_eq!(
            record_wake_context(true, true, true),
            RecordWakeContext::Interrupt
        );
    }

    #[test]
    fn wake_ready_transition_preserves_early_secondary_records() {
        const OWNER: usize = 7;
        let mailbox = Arc::new(LogMailbox::new(2));
        assert!(mailbox.claim(OWNER));

        let early = mailbox.try_publish(
            1,
            LogRecordMeta::log(1, None),
            format_args!("secondary started\n"),
        );
        assert!(early.published());
        assert_eq!(
            record_wake_context(early.published(), false, mailbox.wake_ready(1)),
            RecordWakeContext::None
        );

        mailbox.mark_wake_ready(1);
        let ready = mailbox.try_publish(
            1,
            LogRecordMeta::log(2, Some(8)),
            format_args!("secondary init OK\n"),
        );
        assert!(ready.published());
        assert_eq!(
            record_wake_context(ready.published(), false, mailbox.wake_ready(1)),
            RecordWakeContext::Task
        );

        let mut reader = mailbox.reader();
        assert!(
            reader
                .take(OWNER)
                .is_some_and(|record| record.record.bytes().ends_with(b"secondary started\r\n"))
        );
        assert!(
            reader
                .take(OWNER)
                .is_some_and(|record| record.record.bytes().ends_with(b"secondary init OK\r\n"))
        );
        assert!(reader.take(OWNER).is_none());
    }
}
