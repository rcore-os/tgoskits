use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use ax_driver::serial::{
    self as ax_serial, Config, ConfigError, DataBits, EmergencyFlushResult, EmergencyWriteResult,
    OwnerId, Parity, RxFlag, RxItem, RxQueue, SerialDevice, SerialIrqHandler, SerialIrqOutcome,
    SerialPort, SerialSoftWork, StopBits, TxQueue,
};
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::{
    console::{RuntimeOutputFlushResultV1, RuntimeOutputResultV1, RuntimeOutputSinkV1},
    hal::{
        console::{ConsoleDeviceIdError, ConsoleDeviceIdResult},
        irq::{AutoEnable, CpuId, IrqAffinity, IrqHandle, IrqId, IrqRequest, ShareMode},
    },
};
use ax_sync::PiMutex;
use axpoll::{IoEvents, PollSet};
use bitflags::bitflags;
use rdrive::DeviceId as RDriveDeviceId;
use spin::LazyLock;
use starry_process::Process;

use super::{
    Tty,
    serial_start::{FailedStartRecovery, SerialStartMode, SerialStartPolicy},
    terminal::{
        Terminal,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
        termios::{Termios2, TermiosParity},
    },
};
use crate::{pseudofs::DeviceOps, task::future::IrqNotify};

pub type SerialTtyDriver = Tty<SerialReader, SerialWriter>;

const SERIAL_RX_DRAIN_CHUNK: usize = 256;
const SERIAL_SYNC_ECHO_LIMIT: usize = 256;
const SERIAL_EVENT_BATCH_LIMIT: usize = 64;

bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct SerialEventBits: u32 {
        const RX_READY = 1 << 0;
        const TX_SPACE = 1 << 1;
        const HANGUP   = 1 << 2;
        const RESERVICE = 1 << 3;
    }
}

pub struct SerialTtyEntry {
    number: usize,
    tty: Arc<SerialTtyDriver>,
    backend: Arc<SerialBackend>,
}

/// Proof that the selected boot console is reserved for an adopt-only start and
/// bound as the init process's controlling terminal.
#[must_use = "dropping the token leaves early console output ownership unchanged"]
pub struct PreparedConsoleHandover {
    backend: Arc<SerialBackend>,
    committed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PortStartError {
    Failed,
    RecoveryFailed,
}

impl PreparedConsoleHandover {
    /// Irreversibly transfers console output ownership to the runtime TTY.
    ///
    /// Construction performs no UART access. Commit first publishes a prepared
    /// runtime route, pauses early output, and starts the runtime port without
    /// recomputing the line rate. The final IRQ-off boundary retires the early
    /// path before publishing the runtime route as committed. A recoverable
    /// startup error restores boot polling; an unprovable rollback fails closed
    /// and never re-enters the early register owner.
    pub fn commit(mut self) -> AxResult<()> {
        // SAFETY: the registry retains this backend until shutdown. Both
        // callbacks are bounded, use only preallocated queues, and never call
        // the retired platform console.
        let runtime_output = unsafe {
            ax_runtime::console::prepare_runtime_output_sink(self.backend.runtime_output_sink())
        }
        .map_err(|_| AxError::ResourceBusy)?;
        let platform_handover = ax_runtime::hal::console::prepare_runtime_output_handover()
            .map_err(|_| AxError::ResourceBusy)?;
        match self.backend.commit_console_handover() {
            Ok(()) => {}
            Err(PortStartError::Failed) => {
                drop(platform_handover);
                drop(runtime_output);
                warn!("{} console takeover failed", self.backend.tty_name);
                return Err(AxError::Unsupported);
            }
            Err(PortStartError::RecoveryFailed) => {
                // The device may still have live runtime IRQ state. The
                // platform token cannot restore polling without creating two
                // register owners, while the runtime writer is not proven
                // usable. Retire early access and publish a fail-closed route
                // as one local IRQ-off transition.
                let _platform_result = with_local_irqs_disabled(|| {
                    let platform_result = platform_handover.commit();
                    runtime_output.fail_closed();
                    platform_result
                });
                return Err(AxError::BadState);
            }
        }
        let (platform_result, runtime_result) = with_local_irqs_disabled(|| {
            // PREPARED already routes output to the now-writable runtime
            // backend. Retire the paused early owner first, then Release-publish
            // COMMITTED; no intermediate state can fall back to early MMIO.
            let platform_result = platform_handover.commit();
            let runtime_result = runtime_output.commit();
            (platform_result, runtime_result)
        });
        if platform_result.is_err() || runtime_result.is_err() {
            return Err(AxError::BadState);
        }
        self.backend.complete_console_handover();
        self.committed = true;
        Ok(())
    }
}

fn with_local_irqs_disabled<R>(operation: impl FnOnce() -> R) -> R {
    let restore_irqs = ax_runtime::hal::asm::irqs_enabled();
    ax_runtime::hal::asm::disable_irqs();
    let result = operation();
    if restore_irqs {
        ax_runtime::hal::asm::enable_irqs();
    }
    result
}

impl Drop for PreparedConsoleHandover {
    fn drop(&mut self) {
        if !self.committed {
            self.backend.cancel_console_handover();
        }
    }
}

impl SerialTtyEntry {
    pub fn number(&self) -> usize {
        self.number
    }

    pub fn tty(&self) -> Arc<SerialTtyDriver> {
        self.tty.clone()
    }
}

struct SerialRegistry {
    entries: Vec<SerialTtyEntry>,
    console_index: Option<usize>,
}

struct SerialBackend {
    name: String,
    tty_name: String,
    rdrive_device_id: RDriveDeviceId,
    number: usize,
    owner: OwnerId,
    port: Arc<SerialPort>,
    tx: SpinNoIrq<TxQueue>,
    rx: SpinNoIrq<RxQueue>,
    irq: IrqId,
    irq_handle: SpinNoIrq<Option<IrqHandle>>,
    start_policy: SerialStartPolicy,
    console_handover_prepared: AtomicBool,
    started: AtomicBool,
    start_lock: PiMutex<()>,
    events: SerialEvents,
    input_source: Arc<PollSet>,
    output_source: Arc<PollSet>,
    tx_notify: IrqNotify,
    output_lock: PiMutex<()>,
}

struct SerialEvents {
    pending: AtomicU32,
    notify: IrqNotify,
}

unsafe extern "C" fn runtime_normal_output(
    context: usize,
    bytes: *const u8,
    len: usize,
) -> RuntimeOutputResultV1 {
    // SAFETY: descriptor publication guarantees a live SerialBackend and a
    // readable callback slice for this call.
    let backend = unsafe { &*(context as *const SerialBackend) };
    let bytes = unsafe { core::slice::from_raw_parts(bytes, len) };
    let written = backend.try_submit_runtime_output(bytes);
    if written == 0 {
        RuntimeOutputResultV1::busy()
    } else {
        RuntimeOutputResultV1::progress(written)
    }
}

unsafe extern "C" fn runtime_emergency_output(
    context: usize,
    bytes: *const u8,
    len: usize,
) -> RuntimeOutputResultV1 {
    // SAFETY: descriptor publication guarantees a live SerialBackend and a
    // readable callback slice for this call.
    let backend = unsafe { &*(context as *const SerialBackend) };
    let bytes = unsafe { core::slice::from_raw_parts(bytes, len) };

    // The portable owner gate accepts any CPU only while the UART is idle and
    // rejects concurrent or recursive access. Excluding local IRQ delivery
    // closes the same-CPU normal-owner reentry window without entering
    // LockRuntime, the scheduler, or the normal TX queue.
    let restore_irqs = ax_runtime::hal::asm::irqs_enabled();
    ax_runtime::hal::asm::disable_irqs();
    let result = backend.port.try_write_emergency(bytes);
    if restore_irqs {
        ax_runtime::hal::asm::enable_irqs();
    }

    match result {
        EmergencyWriteResult::Written { count } if count > 0 => {
            RuntimeOutputResultV1::progress(count)
        }
        EmergencyWriteResult::Written { .. } | EmergencyWriteResult::Busy => {
            RuntimeOutputResultV1::busy()
        }
        EmergencyWriteResult::Fault => RuntimeOutputResultV1::failed(),
    }
}

unsafe extern "C" fn runtime_emergency_flush(context: usize) -> RuntimeOutputFlushResultV1 {
    // SAFETY: descriptor publication guarantees a live SerialBackend.
    let backend = unsafe { &*(context as *const SerialBackend) };
    let restore_irqs = ax_runtime::hal::asm::irqs_enabled();
    ax_runtime::hal::asm::disable_irqs();
    let result = backend.port.try_flush_emergency();
    if restore_irqs {
        ax_runtime::hal::asm::enable_irqs();
    }

    match result {
        EmergencyFlushResult::Flushed => RuntimeOutputFlushResultV1::flushed(),
        EmergencyFlushResult::Busy => RuntimeOutputFlushResultV1::busy(),
        EmergencyFlushResult::Fault => RuntimeOutputFlushResultV1::failed(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SerialEventBatch {
    Drained,
    Deferred,
}

fn serial_soft_work_for_events(pending: SerialEventBits) -> SerialSoftWork {
    let mut work = SerialSoftWork::empty();
    if pending.contains(SerialEventBits::TX_SPACE) {
        work |= SerialSoftWork::TX_KICK;
    }
    if pending.contains(SerialEventBits::RESERVICE) {
        work |= SerialSoftWork::RESERVICE;
    }
    work
}

fn process_serial_event_batch(
    mut take_pending: impl FnMut() -> SerialEventBits,
    mut publish_pending: impl FnMut(SerialEventBits),
    mut observe_ready: impl FnMut(SerialEventBits),
    mut service: impl FnMut(SerialSoftWork) -> SerialIrqOutcome,
) -> SerialEventBatch {
    for _ in 0..SERIAL_EVENT_BATCH_LIMIT {
        let pending = take_pending();
        if pending.is_empty() {
            return SerialEventBatch::Drained;
        }
        observe_ready(pending);

        let work = serial_soft_work_for_events(pending);
        if !work.is_empty() {
            publish_pending(serial_event_bits_for_outcome(service(work)));
        }
    }
    SerialEventBatch::Deferred
}

impl SerialEvents {
    const fn new() -> Self {
        Self {
            pending: AtomicU32::new(0),
            notify: IrqNotify::new(),
        }
    }

    fn publish_irq(&self, events: SerialEventBits) {
        if events.is_empty() {
            return;
        }
        self.pending.fetch_or(events.bits(), Ordering::Release);
        self.notify.notify_irq();
    }

    fn publish(&self, events: SerialEventBits) {
        if events.is_empty() {
            return;
        }
        self.pending.fetch_or(events.bits(), Ordering::Release);
        self.notify.notify();
    }

    fn wait(&self) {
        self.notify.wait();
    }

    fn take(&self) -> SerialEventBits {
        SerialEventBits::from_bits_retain(self.pending.swap(0, Ordering::AcqRel))
    }
}

struct NoConsole;

impl DeviceOps for NoConsole {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> AxResult<usize> {
        Err(AxError::NoSuchDevice)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> AxResult<usize> {
        Err(AxError::NoSuchDevice)
    }

    fn ioctl(&self, _cmd: u32, _arg: usize) -> AxResult<usize> {
        Err(AxError::NoSuchDevice)
    }

    fn open(&self, _exclusive: bool) -> AxResult<()> {
        Err(AxError::NoSuchDevice)
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

#[derive(Clone, Copy)]
struct ConsoleCandidate {
    number: usize,
    device_id: RDriveDeviceId,
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum ConsoleSelection {
    SelectedDevice(usize),
    TtyS0Fallback(usize),
}

impl ConsoleSelection {
    fn index(&self) -> usize {
        match self {
            Self::SelectedDevice(index) | Self::TtyS0Fallback(index) => *index,
        }
    }
}

#[derive(Clone)]
pub struct SerialReader {
    backend: Arc<SerialBackend>,
}

#[derive(Clone)]
pub struct SerialWriter {
    backend: Arc<SerialBackend>,
}

static SERIAL_REGISTRY: LazyLock<SerialRegistry> = LazyLock::new(SerialRegistry::discover);

pub fn serial_tty_entries() -> &'static [SerialTtyEntry] {
    &SERIAL_REGISTRY.entries
}

impl SerialTtyDriver {
    pub fn serial_number(&self) -> usize {
        self.writer.backend.number
    }
}

pub fn console_device() -> Arc<dyn DeviceOps> {
    SERIAL_REGISTRY
        .console_index
        .and_then(|index| SERIAL_REGISTRY.entries.get(index))
        .map(|entry| entry.tty() as Arc<dyn DeviceOps>)
        .unwrap_or_else(|| Arc::new(NoConsole))
}

pub fn prepare_console_handover(proc: &Process) -> AxResult<PreparedConsoleHandover> {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        let handover = entry.backend.prepare_console_handover()?;
        entry.tty.bind_to(proc)?;
        return Ok(handover);
    }
    Err(AxError::NoSuchDevice)
}

impl SerialRegistry {
    fn discover() -> Self {
        let serials = ax_serial::take_serial_devices();
        let numbers = assign_tty_numbers(
            serials
                .iter()
                .map(|serial| serial.info.alias_index)
                .collect::<Vec<_>>()
                .as_slice(),
        );

        let mut entries = Vec::new();
        for (serial, number) in serials.into_iter().zip(numbers) {
            let Some(number) = number else {
                warn!(
                    "Skipping serial device {} at {} because ttyS number could not be assigned",
                    serial.name, serial.info.fdt_path
                );
                continue;
            };
            match new_serial_tty(number, serial) {
                Ok(entry) => entries.push(entry),
                Err(err) => warn!("Skipping ttyS{number}: {err:?}"),
            }
        }
        entries.sort_by_key(|entry| entry.number);

        let candidates = entries
            .iter()
            .map(|entry| ConsoleCandidate {
                number: entry.number,
                device_id: entry.backend.rdrive_device_id,
            })
            .collect::<Vec<_>>();
        let console_selection =
            select_console_candidate(&candidates, ax_runtime::hal::console::device_id());
        let console_index = console_selection.as_ref().map(ConsoleSelection::index);
        for (index, entry) in entries.iter().enumerate() {
            let mode = if Some(index) == console_index {
                SerialStartMode::AdoptBootConfiguration
            } else {
                SerialStartMode::ConfigurePort
            };
            entry
                .backend
                .start_policy
                .assign(mode)
                .expect("serial startup policy must be assigned exactly once");
        }
        if let Some(index) = console_index {
            let number = entries[index].number;
            match console_selection {
                Some(ConsoleSelection::SelectedDevice(_)) => {
                    info!("/dev/console bound to ttyS{number}");
                }
                Some(ConsoleSelection::TtyS0Fallback(_)) => {
                    info!("/dev/console bound to ttyS0");
                }
                None => {}
            }
        } else {
            warn!("/dev/console has no serial TTY binding");
        }

        Self {
            entries,
            console_index,
        }
    }
}

fn new_serial_tty(number: usize, serial: SerialDevice) -> AxResult<SerialTtyEntry> {
    let tty_name = format!("ttyS{number}");
    let SerialDevice {
        name,
        rdrive_device_id,
        info,
        runtime,
    } = serial;
    let Some(irq_binding) = info.irq.clone() else {
        return Err(AxError::Unsupported);
    };
    let irq_id = ax_runtime::irq::resolve_binding_irq(irq_binding).map_err(|err| {
        warn!(
            "Failed to resolve {} IRQ binding for {}: {err:?}",
            tty_name, info.fdt_path
        );
        AxError::Unsupported
    })?;
    let port = runtime.port;
    let tx = runtime.tx;
    let rx = runtime.rx;
    let irq = runtime.irq;
    let owner = port.owner();
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        rdrive_device_id,
        number,
        owner,
        port,
        tx: SpinNoIrq::new(tx),
        rx: SpinNoIrq::new(rx),
        irq: irq_id,
        irq_handle: SpinNoIrq::new(None),
        start_policy: SerialStartPolicy::new(),
        console_handover_prepared: AtomicBool::new(false),
        started: AtomicBool::new(false),
        start_lock: PiMutex::new(()),
        events: SerialEvents::new(),
        input_source: Arc::new(PollSet::new()),
        output_source: Arc::new(PollSet::new()),
        tx_notify: IrqNotify::new(),
        output_lock: PiMutex::new(()),
    });

    backend.register_irq(irq)?;
    spawn_serial_event_worker(backend.clone());

    let terminal = Arc::new(Terminal::default());
    let entry_backend = backend.clone();
    let tty = Tty::new(
        terminal,
        TtyConfig {
            reader: SerialReader {
                backend: backend.clone(),
            },
            writer: SerialWriter { backend },
            process_mode: ProcessMode::InterruptDriven {
                input: entry_backend.input_source.clone(),
                output: Some(entry_backend.output_source.clone()),
            },
        },
    );
    info!(
        "{} registered: path={}, alias={:?}, paddr={:#x}, mapped={:#x}, irq={:?}, mode=interrupt",
        tty_name, info.fdt_path, info.alias_index, info.paddr, info.mapped_base, irq_id
    );
    Ok(SerialTtyEntry {
        number,
        tty,
        backend: entry_backend,
    })
}

impl SerialBackend {
    fn runtime_output_sink(&self) -> RuntimeOutputSinkV1 {
        RuntimeOutputSinkV1::new(
            self as *const Self as usize,
            runtime_normal_output,
            runtime_emergency_output,
            runtime_emergency_flush,
        )
    }

    fn try_submit_runtime_output(&self, bytes: &[u8]) -> usize {
        if bytes.is_empty() || !self.started.load(Ordering::Acquire) {
            return 0;
        }
        let Some(mut tx) = self.tx.try_lock() else {
            return 0;
        };
        let submitted = tx.submit(bytes);
        drop(tx);

        if submitted.accepted > 0 {
            self.events.publish_irq(SerialEventBits::RESERVICE);
        }
        submitted.accepted
    }

    fn register_irq(self: &Arc<Self>, mut irq: SerialIrqHandler) -> AxResult<()> {
        let backend = self.clone();
        let request = IrqRequest::new(move |ctx| {
            let outcome = backend.handle_irq_on_owner(ctx.cpu, &mut irq);
            if !outcome.claimed {
                return ax_runtime::hal::irq::IrqReturn::Unhandled;
            }
            let events = publish_serial_outcome(&backend, outcome, true);
            if events.is_empty() {
                ax_runtime::hal::irq::IrqReturn::Handled
            } else {
                ax_runtime::hal::irq::IrqReturn::Wake
            }
        })
        .share_mode(ShareMode::Shared)
        .affinity(IrqAffinity::Fixed(CpuId(self.owner.0)))
        .auto_enable(AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(self.irq, request) {
            Ok(handle) => {
                *self.irq_handle.lock() = Some(handle);
                Ok(())
            }
            Err(err) => {
                warn!(
                    "Failed to register {} IRQ handler for irq {:?}: {err:?}",
                    self.tty_name, self.irq
                );
                Err(AxError::Unsupported)
            }
        }
    }

    fn start_port(&self) -> Result<(), PortStartError> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        let _guard = self.start_lock.lock();
        self.start_port_locked()
    }

    fn try_start_console_port(&self) -> Result<(), PortStartError> {
        let Some(_guard) = self.start_lock.try_lock() else {
            return Err(PortStartError::Failed);
        };
        self.start_port_locked()
    }

    fn start_port_locked(&self) -> Result<(), PortStartError> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }

        let Some(handle) = *self.irq_handle.lock() else {
            return Err(PortStartError::Failed);
        };

        let Ok(mode) = self.start_policy.mode() else {
            return Err(PortStartError::Failed);
        };
        let config = match mode.startup_baudrate(|| self.baudrate()) {
            Some(baudrate) => Config::new().baudrate(baudrate),
            None => Config::new(),
        };
        if let Err(err) = self.startup_port(&config) {
            let recovered = self.abort_failed_start(mode);
            if mode == SerialStartMode::ConfigurePort {
                warn!(
                    "{} failed to start serial port {}: {:?}",
                    self.tty_name, self.name, err
                );
            }
            return Err(if recovered {
                PortStartError::Failed
            } else {
                PortStartError::RecoveryFailed
            });
        }

        if let Err(err) = ax_runtime::hal::irq::enable_irq(handle) {
            let recovered = self.abort_failed_start(mode);
            let _ = ax_runtime::hal::irq::disable_irq(handle);
            let _ = ax_runtime::hal::irq::synchronize_irq(handle);
            if mode == SerialStartMode::ConfigurePort {
                warn!(
                    "Failed to enable {} IRQ handler for irq {:?}: {err:?}",
                    self.tty_name, self.irq
                );
            }
            return Err(if recovered {
                PortStartError::Failed
            } else {
                PortStartError::RecoveryFailed
            });
        }

        self.started.store(true, Ordering::Release);
        if mode == SerialStartMode::ConfigurePort {
            self.publish_started_events();
        }
        Ok(())
    }

    fn abort_failed_start(&self, mode: SerialStartMode) -> bool {
        match mode.failed_start_recovery() {
            FailedStartRecovery::RestoreBootPolling => {
                ax_serial::run_on_owner(self.owner, |lease| self.port.quiesce_to_polling(lease))
                    .is_ok()
            }
            FailedStartRecovery::ShutdownPort => self.shutdown_port(),
        }
    }

    fn publish_started_events(&self) {
        publish_serial_outcome(
            self,
            self.service_on_owner(SerialSoftWork::RESERVICE),
            false,
        );
        self.events.publish(SerialEventBits::RX_READY);
    }

    fn ensure_started(&self) -> AxResult<()> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.start_policy.mode() != Ok(SerialStartMode::ConfigurePort) {
            return Err(AxError::ResourceBusy);
        }
        match self.start_port() {
            Ok(()) => Ok(()),
            Err(PortStartError::Failed) => Err(AxError::Unsupported),
            Err(PortStartError::RecoveryFailed) => {
                panic!("serial startup rollback failed for {}", self.tty_name)
            }
        }
    }

    fn open(&self) -> AxResult<()> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        match self.start_policy.mode() {
            Ok(SerialStartMode::ConfigurePort) => self.ensure_started(),
            Ok(SerialStartMode::AdoptBootConfiguration)
                if self.console_handover_prepared.load(Ordering::Acquire) =>
            {
                Ok(())
            }
            Ok(SerialStartMode::AdoptBootConfiguration) => Err(AxError::ResourceBusy),
            Err(_) => Err(AxError::Unsupported),
        }
    }

    fn prepare_console_handover(self: &Arc<Self>) -> AxResult<PreparedConsoleHandover> {
        if self.start_policy.mode() != Ok(SerialStartMode::AdoptBootConfiguration)
            || self.started.load(Ordering::Acquire)
        {
            return Err(AxError::ResourceBusy);
        }
        self.console_handover_prepared
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AxError::ResourceBusy)?;
        Ok(PreparedConsoleHandover {
            backend: Arc::clone(self),
            committed: false,
        })
    }

    fn commit_console_handover(&self) -> Result<(), PortStartError> {
        if !self.console_handover_prepared.load(Ordering::Acquire)
            || self.start_policy.mode() != Ok(SerialStartMode::AdoptBootConfiguration)
        {
            return Err(PortStartError::Failed);
        }
        self.try_start_console_port()
    }

    fn complete_console_handover(&self) {
        self.console_handover_prepared
            .store(false, Ordering::Release);
        self.publish_started_events();
    }

    fn cancel_console_handover(&self) {
        if !self.started.load(Ordering::Acquire) {
            self.console_handover_prepared
                .store(false, Ordering::Release);
        }
    }

    fn startup_port(&self, config: &Config) -> Result<SerialIrqOutcome, ConfigError> {
        ax_serial::run_on_owner(self.owner, |lease| self.port.startup(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    fn shutdown_port(&self) -> bool {
        ax_serial::run_on_owner(self.owner, |lease| self.port.shutdown(lease)).is_ok()
    }

    fn set_port_config(&self, config: &Config) -> Result<(), ConfigError> {
        ax_serial::run_on_owner(self.owner, |lease| self.port.set_config(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    fn baudrate(&self) -> u32 {
        ax_serial::run_on_owner(self.owner, |lease| self.port.baudrate(lease)).unwrap_or(0)
    }

    fn service_on_owner(&self, work: SerialSoftWork) -> SerialIrqOutcome {
        ax_serial::run_on_owner(self.owner, |lease| self.port.service(lease, work))
            .unwrap_or_default()
    }

    fn handle_irq_on_owner(&self, cpu: CpuId, irq: &mut SerialIrqHandler) -> SerialIrqOutcome {
        let Some(lease) = ax_serial::owner_lease_for_cpu(self.owner, cpu) else {
            return SerialIrqOutcome::default();
        };
        irq.handle(lease)
    }

    fn submit_tx(&self, bytes: &[u8]) -> (usize, SerialIrqOutcome) {
        let submit = self.tx.lock().submit(bytes);
        let outcome = if submit.needs_kick {
            self.service_on_owner(SerialSoftWork::TX_KICK)
        } else {
            SerialIrqOutcome::default()
        };
        (submit.accepted, outcome)
    }

    fn tx_idle(&self) -> bool {
        ax_serial::run_on_owner(self.owner, |lease| self.port.tx_idle(lease)).unwrap_or(false)
    }

    fn drain_tx(&self) -> AxResult<()> {
        self.ensure_started()?;
        let _guard = self.output_lock.lock();
        loop {
            let outcome = self.service_on_owner(SerialSoftWork::TX_KICK);
            publish_serial_outcome(self, outcome, false);
            if self.tx_idle() {
                return Ok(());
            }
            crate::task::sleep(Duration::from_millis(1));
        }
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.rx.lock().drain(out)
    }
}

fn serial_config_from_termios(termios: &Termios2) -> Config {
    let mut config = Config::new()
        .data_bits(match termios.data_bits() {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        })
        .stop_bits(if termios.stop_bits() == 2 {
            StopBits::Two
        } else {
            StopBits::One
        })
        .parity(match termios.parity() {
            TermiosParity::None => Parity::None,
            TermiosParity::Odd => Parity::Odd,
            TermiosParity::Even => Parity::Even,
            TermiosParity::Mark => Parity::Mark,
            TermiosParity::Space => Parity::Space,
        });
    if let Some(baudrate) = termios.baudrate() {
        config = config.baudrate(baudrate);
    }
    config
}

fn spawn_serial_event_worker(backend: Arc<SerialBackend>) {
    let task_name = format!("{}-event", backend.tty_name);
    crate::task::spawn_kernel_thread(
        move || loop {
            backend.events.wait();
            let disposition = process_serial_event_batch(
                || backend.events.take(),
                |events| backend.events.publish(events),
                |pending| {
                    if pending.contains(SerialEventBits::RX_READY) {
                        unsafe { backend.input_source.wake(IoEvents::IN) };
                    }
                    if pending.contains(SerialEventBits::TX_SPACE) {
                        backend.tx_notify.notify();
                        unsafe { backend.output_source.wake(IoEvents::OUT) };
                    }
                },
                |work| backend.service_on_owner(work),
            );
            if disposition == SerialEventBatch::Deferred {
                crate::task::yield_now();
            }
        },
        task_name,
    );
}

fn publish_serial_outcome(
    backend: &SerialBackend,
    outcome: SerialIrqOutcome,
    from_irq: bool,
) -> SerialEventBits {
    let events = serial_event_bits_for_outcome(outcome);

    if from_irq {
        backend.events.publish_irq(events);
    } else {
        backend.events.publish(events);
    }
    events
}

fn serial_event_bits_for_outcome(outcome: SerialIrqOutcome) -> SerialEventBits {
    let mut events = SerialEventBits::empty();
    if outcome.rx_pushed > 0 {
        events |= SerialEventBits::RX_READY;
    }
    if outcome.tx_wakeup {
        events |= SerialEventBits::TX_SPACE;
    }
    if outcome.budget_exhausted {
        events |= SerialEventBits::RESERVICE;
    }
    events
}

impl TtyRead for SerialReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        if !self.backend.started.load(Ordering::Acquire) {
            return 0;
        }

        let mut total = 0;
        let mut temp = [RxItem::default(); SERIAL_RX_DRAIN_CHUNK];

        while total < buf.len() {
            let limit = (buf.len() - total).min(temp.len());
            let read = self.backend.drain_rx(&mut temp[..limit]);
            if read == 0 {
                break;
            }
            for item in &temp[..read] {
                match *item {
                    RxItem::Byte {
                        byte,
                        flag: RxFlag::Normal,
                    } => {
                        buf[total] = byte;
                        total += 1;
                    }
                    RxItem::Byte { byte, flag } => {
                        warn!(
                            "{} RX error {:?} while preserving byte {byte:#x}",
                            self.backend.tty_name, flag
                        );
                        buf[total] = byte;
                        total += 1;
                    }
                    RxItem::Overrun => {
                        warn!("{} RX overrun", self.backend.tty_name);
                    }
                }
            }
        }

        total
    }
}

impl TtyWrite for SerialWriter {
    fn open(&self) -> AxResult<()> {
        self.backend.open()
    }

    fn write(&self, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        if self.backend.ensure_started().is_err() {
            return;
        }
        let _guard = self.backend.output_lock.lock();
        let mut written = 0;
        while written < buf.len() {
            let (count, outcome) = self.backend.submit_tx(&buf[written..]);
            publish_serial_outcome(&self.backend, outcome, false);
            if count == 0 {
                self.backend.tx_notify.wait();
                continue;
            }
            written += count;
        }
    }

    fn try_write(&self, buf: &[u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        if self.backend.ensure_started().is_err() {
            return 0;
        }
        let Some(_guard) = self.backend.output_lock.try_lock() else {
            return 0;
        };
        let (count, outcome) = self.backend.submit_tx(buf);
        publish_serial_outcome(&self.backend, outcome, false);
        count
    }

    fn flush_echo_before_input(&self) -> bool {
        true
    }

    fn max_sync_echo_bytes(&self) -> usize {
        SERIAL_SYNC_ECHO_LIMIT
    }

    fn drain(&self) -> AxResult<()> {
        self.backend.drain_tx()
    }

    fn termios_changed(&self, old: &Termios2, new: &Termios2) {
        if old.baudrate() == new.baudrate()
            && old.data_bits() == new.data_bits()
            && old.stop_bits() == new.stop_bits()
            && old.parity() == new.parity()
        {
            return;
        }
        if self.backend.ensure_started().is_err() {
            return;
        }
        if let Err(err) = self
            .backend
            .set_port_config(&serial_config_from_termios(new))
        {
            warn!(
                "{} failed to apply termios on {}: {:?}",
                self.backend.tty_name, self.backend.name, err
            );
        }
    }
}

fn assign_tty_numbers(alias_indices: &[Option<usize>]) -> Vec<Option<usize>> {
    let mut assigned = vec![None; alias_indices.len()];
    let mut used = Vec::new();

    for (device_index, alias) in alias_indices.iter().copied().enumerate() {
        let Some(number) = alias else {
            continue;
        };
        if used.contains(&number) {
            warn!("Duplicate FDT serial{number} alias ignored for later serial device");
            continue;
        }
        assigned[device_index] = Some(number);
        used.push(number);
    }

    let mut next = 0usize;
    for number in &mut assigned {
        if number.is_some() {
            continue;
        }
        while used.contains(&next) {
            next += 1;
        }
        *number = Some(next);
        used.push(next);
    }

    assigned
}

fn select_console_candidate(
    candidates: &[ConsoleCandidate],
    selected_device_id: ConsoleDeviceIdResult,
) -> Option<ConsoleSelection> {
    match selected_device_id {
        Ok(device_id) => {
            if let Some(index) = candidates
                .iter()
                .position(|candidate| candidate.device_id == device_id)
            {
                return Some(ConsoleSelection::SelectedDevice(index));
            }
            warn!("selected console device {device_id:?} did not match a discovered serial TTY");
            None
        }
        Err(ConsoleDeviceIdError::NotSpecified) => candidates
            .iter()
            .position(|candidate| candidate.number == 0)
            .map(ConsoleSelection::TtyS0Fallback),
        Err(
            err @ (ConsoleDeviceIdError::NoHardwareDevice | ConsoleDeviceIdError::DeviceNotFound),
        ) => {
            debug!("No hardware console TTY selected: {err:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use rdrive::DeviceId as RDriveDeviceId;

    use super::{
        ConsoleCandidate, ConsoleDeviceIdError, ConsoleSelection, SERIAL_EVENT_BATCH_LIMIT,
        SerialEventBatch, SerialEventBits, SerialIrqOutcome, SerialSoftWork, assign_tty_numbers,
        process_serial_event_batch, select_console_candidate, serial_event_bits_for_outcome,
        serial_soft_work_for_events,
    };

    #[test]
    fn event_worker_defers_and_republishes_multi_budget_reservice() {
        use core::cell::Cell;

        let pending_bits = Cell::new(SerialEventBits::RESERVICE.bits());
        let service_calls = Cell::new(0usize);
        let take_pending = || SerialEventBits::from_bits_retain(pending_bits.replace(0));
        let publish_pending = |events: SerialEventBits| {
            pending_bits.set(pending_bits.get() | events.bits());
        };
        let service = |work: SerialSoftWork| {
            assert!(work.contains(SerialSoftWork::RESERVICE));
            let call = service_calls.get() + 1;
            service_calls.set(call);
            SerialIrqOutcome {
                claimed: true,
                rx_pushed: 0,
                tx_sent: 64,
                tx_wakeup: false,
                budget_exhausted: call < SERIAL_EVENT_BATCH_LIMIT + 2,
            }
        };

        assert_eq!(
            process_serial_event_batch(take_pending, publish_pending, |_| {}, service),
            SerialEventBatch::Deferred
        );
        assert_eq!(service_calls.get(), SERIAL_EVENT_BATCH_LIMIT);
        assert_ne!(pending_bits.get() & SerialEventBits::RESERVICE.bits(), 0);

        assert_eq!(
            process_serial_event_batch(take_pending, publish_pending, |_| {}, service),
            SerialEventBatch::Drained
        );
        assert_eq!(service_calls.get(), SERIAL_EVENT_BATCH_LIMIT + 2);
        assert_eq!(pending_bits.get(), 0);
    }

    #[test]
    fn event_worker_maps_tx_and_reservice_work_independently() {
        assert_eq!(
            serial_soft_work_for_events(SerialEventBits::TX_SPACE),
            SerialSoftWork::TX_KICK
        );
        assert_eq!(
            serial_soft_work_for_events(SerialEventBits::RESERVICE),
            SerialSoftWork::RESERVICE
        );
        assert_eq!(
            serial_soft_work_for_events(SerialEventBits::TX_SPACE | SerialEventBits::RESERVICE),
            SerialSoftWork::TX_KICK | SerialSoftWork::RESERVICE
        );
    }

    #[test]
    fn exhausted_irq_budget_requests_task_context_reservice() {
        let outcome = SerialIrqOutcome {
            claimed: true,
            rx_pushed: 3,
            tx_sent: 5,
            tx_wakeup: true,
            budget_exhausted: true,
        };

        assert_eq!(
            serial_event_bits_for_outcome(outcome),
            SerialEventBits::RX_READY | SerialEventBits::TX_SPACE | SerialEventBits::RESERVICE
        );
    }

    #[test]
    fn aliases_keep_linux_ttys_numbering() {
        assert_eq!(assign_tty_numbers(&[Some(0), Some(2)]), [Some(0), Some(2)]);
    }

    #[test]
    fn unaliased_serials_take_first_free_ttys_numbers() {
        assert_eq!(
            assign_tty_numbers(&[Some(0), None, Some(2), None]),
            [Some(0), Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn duplicate_alias_keeps_first_device_and_reassigns_later_one() {
        assert_eq!(
            assign_tty_numbers(&[Some(1), Some(1), None]),
            [Some(1), Some(0), Some(2)]
        );
    }

    #[test]
    fn matching_device_id_wins_over_ttys0_fallback() {
        let tty_s0 = RDriveDeviceId::from(10);
        let tty_s1 = RDriveDeviceId::from(11);
        let candidates = [
            ConsoleCandidate {
                number: 0,
                device_id: tty_s0,
            },
            ConsoleCandidate {
                number: 1,
                device_id: tty_s1,
            },
        ];

        assert_eq!(
            select_console_candidate(&candidates, Ok(tty_s1)),
            Some(ConsoleSelection::SelectedDevice(1))
        );
    }

    #[test]
    fn unmatched_device_id_keeps_dev_console_unbound() {
        let tty_s0 = RDriveDeviceId::from(10);
        let missing = RDriveDeviceId::from(99);
        let candidates = [ConsoleCandidate {
            number: 0,
            device_id: tty_s0,
        }];

        assert_eq!(select_console_candidate(&candidates, Ok(missing)), None);
    }

    #[test]
    fn missing_device_id_falls_back_to_ttys0() {
        let tty_s0 = RDriveDeviceId::from(10);
        let candidates = [ConsoleCandidate {
            number: 0,
            device_id: tty_s0,
        }];

        assert_eq!(
            select_console_candidate(&candidates, Err(ConsoleDeviceIdError::NotSpecified)),
            Some(ConsoleSelection::TtyS0Fallback(0))
        );
    }

    #[test]
    fn no_ttys0_keeps_dev_console_unbound() {
        let tty_s1 = RDriveDeviceId::from(11);
        let candidates = [ConsoleCandidate {
            number: 1,
            device_id: tty_s1,
        }];

        assert_eq!(
            select_console_candidate(&candidates, Err(ConsoleDeviceIdError::NotSpecified)),
            None
        );
    }

    #[test]
    fn non_hardware_console_keeps_dev_console_unbound() {
        let tty_s0 = RDriveDeviceId::from(10);
        let candidates = [ConsoleCandidate {
            number: 0,
            device_id: tty_s0,
        }];

        assert_eq!(
            select_console_candidate(&candidates, Err(ConsoleDeviceIdError::NoHardwareDevice)),
            None
        );
    }
}
