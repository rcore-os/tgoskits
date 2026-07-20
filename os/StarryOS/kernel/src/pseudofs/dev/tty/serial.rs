use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    time::Duration,
};

use ax_driver::serial::{
    self as ax_serial, Config, ConfigError, DataBits, EmergencyFlushResult, EmergencyWriteResult,
    FaultContainment, InterruptMask, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource,
    Parity, RxFlag, RxItem, RxQueue, SerialCore, SerialDevice, SerialIrqEvent, SerialIrqEvents,
    SerialIrqFault, SerialMaskedService, SerialSoftWork, StopBits, TxQueue,
};
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::{
    console::{RuntimeOutputFlushResultV1, RuntimeOutputResultV1, RuntimeOutputSinkV1},
    hal::{
        console::{ConsoleDeviceIdError, ConsoleDeviceIdResult},
        irq::{IrqId, IrqReturn},
    },
    maintenance::{
        DeviceMaintenanceHandle, LocalIrqWake, LocalIrqWakeError, LocalOwnerCell,
        LocalOwnerControl, LocalOwnerIrq, MaintenanceCauses, MaintenanceClosed, MaintenanceError,
        MaintenanceIrqAction, MaintenancePublishResult, MaintenanceRegistrar, MaintenanceSession,
        MaintenanceState, MaintenanceThread, spawn_maintenance_domain,
    },
    task::WaitQueue,
};
use ax_sync::PiMutex;
use axpoll::{IoEvents, PollSet};
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
use crate::pseudofs::DeviceOps;

pub type SerialTtyDriver = Tty<SerialReader, SerialWriter>;

const SERIAL_RX_DRAIN_CHUNK: usize = 256;
const SERIAL_SYNC_ECHO_LIMIT: usize = 256;
const SERIAL_EVENT_BATCH_LIMIT: usize = 64;
const MAINTENANCE_REGISTERING: u8 = 0;
const MAINTENANCE_READY: u8 = 1;
const MAINTENANCE_FAILED: u8 = 2;
const START_IDLE: u8 = 0;
const START_REQUESTED: u8 = 1;
const START_SUCCEEDED: u8 = 2;
const START_FAILED: u8 = 3;
const START_RECOVERY_FAILED: u8 = 4;

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
    tx: SpinNoIrq<TxQueue>,
    rx: SpinNoIrq<RxQueue>,
    irq: IrqId,
    maintenance: SpinNoIrq<Option<DeviceMaintenanceHandle<SerialMaintenanceEvent>>>,
    maintenance_thread: SpinNoIrq<Option<MaintenanceThread>>,
    maintenance_state: AtomicU8,
    maintenance_wait: WaitQueue,
    irq_state: Arc<SerialIrqState>,
    start_policy: SerialStartPolicy,
    console_handover_prepared: AtomicBool,
    started: AtomicBool,
    start_state: AtomicU8,
    start_wait: WaitQueue,
    start_lock: PiMutex<()>,
    pending_config: SpinNoIrq<Option<Config>>,
    pending_rearm: SpinNoIrq<Option<MaskedSource>>,
    input_source: Arc<PollSet>,
    output_source: Arc<PollSet>,
    tx_progress: AtomicU64,
    tx_wait: WaitQueue,
    drain_requested: AtomicBool,
    drain_complete: AtomicBool,
    output_lock: PiMutex<()>,
}

#[derive(Clone, Copy, Debug)]
enum SerialMaintenanceEvent {
    Irq {
        event: SerialIrqEvent,
        masked: Option<MaskedSource>,
    },
    Fault {
        reason: SerialIrqFault,
        containment: FaultContainment,
    },
}

struct SerialIrqState {
    publication_failed: AtomicBool,
    action_disabled: AtomicBool,
    line_quenched: AtomicBool,
}

impl SerialIrqState {
    const fn new() -> Self {
        Self {
            publication_failed: AtomicBool::new(false),
            action_disabled: AtomicBool::new(false),
            line_quenched: AtomicBool::new(false),
        }
    }
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

    let result = backend.emergency_write_without_owner(bytes);

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
    let result = backend.emergency_flush_without_owner();

    match result {
        EmergencyFlushResult::Flushed => RuntimeOutputFlushResultV1::flushed(),
        EmergencyFlushResult::Busy => RuntimeOutputFlushResultV1::busy(),
        EmergencyFlushResult::Fault => RuntimeOutputFlushResultV1::failed(),
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
    let ax_serial::SerialRuntime { core, tx, rx } = runtime;
    let creator_cpu = ax_runtime::task::pin_current_cpu().map_err(|_| AxError::BadState)?;
    let owner_cpu = creator_cpu.cpu().as_u32() as usize;
    let irq_state = Arc::new(SerialIrqState::new());
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        rdrive_device_id,
        number,
        tx: SpinNoIrq::new(tx),
        rx: SpinNoIrq::new(rx),
        irq: irq_id,
        maintenance: SpinNoIrq::new(None),
        maintenance_thread: SpinNoIrq::new(None),
        maintenance_state: AtomicU8::new(MAINTENANCE_REGISTERING),
        maintenance_wait: WaitQueue::new(),
        irq_state,
        start_policy: SerialStartPolicy::new(),
        console_handover_prepared: AtomicBool::new(false),
        started: AtomicBool::new(false),
        start_state: AtomicU8::new(START_IDLE),
        start_wait: WaitQueue::new(),
        start_lock: PiMutex::new(()),
        pending_config: SpinNoIrq::new(None),
        pending_rearm: SpinNoIrq::new(None),
        input_source: Arc::new(PollSet::new()),
        output_source: Arc::new(PollSet::new()),
        tx_progress: AtomicU64::new(0),
        tx_wait: WaitQueue::new(),
        drain_requested: AtomicBool::new(false),
        drain_complete: AtomicBool::new(false),
        output_lock: PiMutex::new(()),
    });

    spawn_serial_maintenance(Arc::clone(&backend), core, owner_cpu)?;
    drop(creator_cpu);
    backend.wait_for_maintenance_registration()?;

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
        let Some(maintenance) = self.try_maintenance_handle() else {
            return 0;
        };
        let Some(mut tx) = self.tx.try_lock() else {
            return 0;
        };
        let submitted = tx.submit(bytes);
        drop(tx);

        if submitted.accepted > 0 {
            let _ = maintenance.publish_cause(MaintenanceCauses::SUBMIT);
        }
        submitted.accepted
    }

    fn emergency_write_without_owner(&self, bytes: &[u8]) -> EmergencyWriteResult {
        // The runtime callback has no owner-thread capability and must never
        // wait for another CPU or touch UART MMIO.
        // TODO(platform-emergency-console): provide a separate panic-console
        // capability whose ownership is explicitly transferred away from this
        // normal runtime port before it may access the UART.
        emergency_write_outcome(self.started.load(Ordering::Acquire), bytes.is_empty())
    }

    fn emergency_flush_without_owner(&self) -> EmergencyFlushResult {
        emergency_flush_outcome(self.started.load(Ordering::Acquire))
    }

    fn install_maintenance(&self, maintenance: DeviceMaintenanceHandle<SerialMaintenanceEvent>) {
        *self.maintenance.lock() = Some(maintenance);
        self.maintenance_state
            .store(MAINTENANCE_READY, Ordering::Release);
        self.maintenance_wait.notify_all();
    }

    fn install_maintenance_thread(&self, maintenance_thread: MaintenanceThread) {
        let previous = self.maintenance_thread.lock().replace(maintenance_thread);
        assert!(
            previous.is_none(),
            "serial maintenance thread must be installed exactly once"
        );
    }

    fn fail_maintenance_registration(&self) {
        self.maintenance_state
            .store(MAINTENANCE_FAILED, Ordering::Release);
        self.maintenance_wait.notify_all();
    }

    fn wait_for_maintenance_registration(&self) -> AxResult<()> {
        self.maintenance_wait.wait_until(|| {
            self.maintenance_state.load(Ordering::Acquire) != MAINTENANCE_REGISTERING
        });
        if self.maintenance_state.load(Ordering::Acquire) == MAINTENANCE_READY {
            Ok(())
        } else {
            Err(AxError::Unsupported)
        }
    }

    fn maintenance_handle(&self) -> AxResult<DeviceMaintenanceHandle<SerialMaintenanceEvent>> {
        self.maintenance
            .lock()
            .as_ref()
            .ok_or(AxError::BadState)?
            .try_clone_task_context()
            .map_err(|_| AxError::BadState)
    }

    fn try_maintenance_handle(&self) -> Option<DeviceMaintenanceHandle<SerialMaintenanceEvent>> {
        self.maintenance
            .try_lock()?
            .as_ref()?
            .try_clone_task_context()
            .ok()
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
        if self.start_policy.mode().is_err() {
            return Err(PortStartError::Failed);
        }
        self.start_state.store(START_REQUESTED, Ordering::Release);
        let maintenance = match self.maintenance_handle() {
            Ok(maintenance) => maintenance,
            Err(_) => {
                self.start_state.store(START_FAILED, Ordering::Release);
                return Err(PortStartError::Failed);
            }
        };
        if maintenance
            .publish_cause(MaintenanceCauses::SUBMIT)
            .is_err()
        {
            self.start_state.store(START_FAILED, Ordering::Release);
            return Err(PortStartError::Failed);
        }
        self.start_wait
            .wait_until(|| self.start_state.load(Ordering::Acquire) != START_REQUESTED);
        match self.start_state.load(Ordering::Acquire) {
            START_SUCCEEDED => Ok(()),
            START_RECOVERY_FAILED => Err(PortStartError::RecoveryFailed),
            _ => Err(PortStartError::Failed),
        }
    }

    fn publish_started_events(&self) {
        if let Ok(maintenance) = self.maintenance_handle() {
            let _ = maintenance.publish_cause(MaintenanceCauses::SUBMIT);
        }
        unsafe {
            self.input_source.wake(IoEvents::IN);
            self.output_source.wake(IoEvents::OUT);
        }
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

    fn set_port_config(&self, config: &Config) -> Result<(), ConfigError> {
        let maintenance = self
            .maintenance_handle()
            .map_err(|_| ConfigError::RegisterError)?;
        *self.pending_config.lock() = Some(config.clone());
        maintenance
            .publish_cause(MaintenanceCauses::SUBMIT)
            .map_err(|_| AxError::BadState)
            .map_err(|_| ConfigError::RegisterError)
    }

    fn submit_tx(&self, bytes: &[u8]) -> usize {
        let Ok(maintenance) = self.maintenance_handle() else {
            return 0;
        };
        let submit = self.tx.lock().submit(bytes);
        if submit.needs_kick {
            let _ = maintenance.publish_cause(MaintenanceCauses::SUBMIT);
        }
        submit.accepted
    }

    fn drain_tx(&self) -> AxResult<()> {
        self.ensure_started()?;
        let _guard = self.output_lock.lock();
        loop {
            self.drain_complete.store(false, Ordering::Release);
            self.drain_requested.store(true, Ordering::Release);
            let observed = self.tx_progress.load(Ordering::Acquire);
            self.maintenance_handle()?
                .publish_cause(MaintenanceCauses::SUBMIT)
                .map_err(|_| AxError::BadState)?;
            if self.drain_complete.load(Ordering::Acquire) {
                return Ok(());
            }
            self.tx_wait
                .wait_timeout_until(Duration::from_millis(1), || {
                    self.drain_complete.load(Ordering::Acquire)
                        || !self.started.load(Ordering::Acquire)
                        || self.tx_progress.load(Ordering::Acquire) != observed
                });
            if !self.started.load(Ordering::Acquire) {
                return Err(AxError::BadState);
            }
        }
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        let drain = self.rx.lock().drain(out);
        if let Some(rearm) = drain.rearm {
            if !self.merge_pending_rearm(rearm) {
                self.irq_state
                    .publication_failed
                    .store(true, Ordering::Release);
            }
            if let Ok(maintenance) = self.maintenance_handle() {
                let _ = maintenance.publish_cause(MaintenanceCauses::SUBMIT);
            }
        }
        drain.count
    }

    fn merge_pending_rearm(&self, source: MaskedSource) -> bool {
        merge_masked_source(&mut self.pending_rearm.lock(), source)
    }

    fn take_pending_rearm(&self) -> Option<MaskedSource> {
        self.pending_rearm.lock().take()
    }

    fn close_serial_admission(&self) {
        self.started.store(false, Ordering::Release);
        self.maintenance_state
            .store(MAINTENANCE_FAILED, Ordering::Release);
        let _ = self.maintenance.lock().take();
        let _ = self.pending_config.lock().take();
        let _ = self.pending_rearm.lock().take();
        if self.start_state.load(Ordering::Acquire) == START_REQUESTED {
            self.start_state.store(START_FAILED, Ordering::Release);
        }
        self.maintenance_wait.notify_all();
        self.start_wait.notify_all();
        self.tx_progress.fetch_add(1, Ordering::AcqRel);
        self.tx_wait.notify_all();
        unsafe {
            self.input_source
                .wake(IoEvents::IN | IoEvents::ERR | IoEvents::HUP);
            self.output_source
                .wake(IoEvents::OUT | IoEvents::ERR | IoEvents::HUP);
        }
    }
}

fn emergency_write_outcome(started: bool, empty: bool) -> EmergencyWriteResult {
    if empty {
        EmergencyWriteResult::Written { count: 0 }
    } else if started {
        EmergencyWriteResult::Busy
    } else {
        EmergencyWriteResult::Fault
    }
}

fn emergency_flush_outcome(started: bool) -> EmergencyFlushResult {
    if started {
        EmergencyFlushResult::Busy
    } else {
        EmergencyFlushResult::Fault
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

fn spawn_serial_maintenance(
    backend: Arc<SerialBackend>,
    core: SerialCore,
    owner_cpu: usize,
) -> AxResult<()> {
    let name = format!("{}-maintenance", backend.tty_name);
    let owner_backend = Arc::clone(&backend);
    let maintenance_thread =
        spawn_maintenance_domain::<SerialMaintenanceEvent, _>(owner_cpu, name, move |registrar| {
            run_serial_maintenance(owner_backend, core, registrar)
        })
        .map_err(|error| {
            warn!("failed to spawn serial maintenance owner: {error}");
            AxError::Unsupported
        })?;
    backend.install_maintenance_thread(maintenance_thread);
    Ok(())
}

fn run_serial_maintenance(
    backend: Arc<SerialBackend>,
    core: SerialCore,
    registrar: MaintenanceRegistrar<SerialMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    let owner_cell = LocalOwnerCell::pin(core);
    let (owner, owner_irq) = registrar
        .local_owner_cell(owner_cell.as_ref())
        .unwrap_or_else(|error| {
            backend.fail_maintenance_registration();
            panic!("serial owner cell failed to bind: {error}")
        });
    let irq_wake = registrar
        .local_irq_wake()
        .inspect_err(|_| backend.fail_maintenance_registration())?;
    let maintenance = registrar.remote_handle();
    let irq_state = Arc::clone(&backend.irq_state);
    let owner_cpu = registrar.owner_cpu();
    let irq_name = format!("{}/serial", backend.tty_name);
    let mut owner_irq = owner_irq;
    let registration = registrar.register_shared_disabled(irq_name, backend.irq, move |context| {
        serial_irq_action(
            context.cpu.0,
            owner_cpu,
            &irq_state,
            &irq_wake,
            &mut owner_irq,
        )
    });
    let registration = match registration {
        Ok(registration) => registration,
        Err(error) => {
            warn!(
                "failed to register {} IRQ {:?} on CPU {}: {error:?}",
                backend.tty_name, backend.irq, owner_cpu
            );
            backend.fail_maintenance_registration();
            let session = registrar.activate()?;
            return close_serial_maintenance(&backend, session, owner_cell, owner, None);
        }
    };
    let session = match registrar.activate() {
        Ok(session) => session,
        Err(error) => {
            backend.fail_maintenance_registration();
            if let Err(close) = registration.close() {
                warn!(
                    "failed to close {} IRQ action after maintenance activation failed: {:?}",
                    backend.tty_name,
                    close.reason()
                );
            }
            return Err(error);
        }
    };
    backend.install_maintenance(maintenance);
    let owner_result = serial_owner_loop(&backend, &session, &owner, &registration);
    let close_result =
        close_serial_maintenance(&backend, session, owner_cell, owner, Some(registration));
    match close_result {
        Ok(closed) => {
            if let Err(error) = owner_result {
                warn!(
                    "{} maintenance owner stopped after a contained failure: {error}",
                    backend.tty_name
                );
            }
            Ok(closed)
        }
        Err(close_error) => Err(close_error),
    }
}

fn serial_irq_action(
    actual_cpu: usize,
    owner_cpu: usize,
    irq_state: &SerialIrqState,
    wake: &LocalIrqWake<SerialMaintenanceEvent>,
    core_irq: &mut LocalOwnerIrq<SerialCore>,
) -> IrqReturn {
    if actual_cpu != owner_cpu {
        irq_state.publication_failed.store(true, Ordering::Release);
        return contain_serial_irq(
            core_irq,
            ax_serial::ContainmentCause::OwnerUnavailable,
            irq_state,
        );
    }
    let capture = match core_irq.with_irq(|core| core.capture_irq()) {
        Ok(capture) => capture,
        Err(_) => {
            irq_state.publication_failed.store(true, Ordering::Release);
            irq_state.line_quenched.store(true, Ordering::Release);
            return IrqReturn::MaskLineAndWake;
        }
    };
    match capture {
        IrqCapture::Unhandled => IrqReturn::Unhandled,
        IrqCapture::Captured { event, masked } => {
            let publication = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                SerialMaintenanceEvent::Irq { event, masked },
            );
            match publication {
                Ok(MaintenancePublishResult::Published) => IrqReturn::Wake,
                Ok(MaintenancePublishResult::Overflowed) => {
                    irq_state.publication_failed.store(true, Ordering::Release);
                    contain_serial_irq(
                        core_irq,
                        ax_serial::ContainmentCause::PublicationFull,
                        irq_state,
                    )
                }
                Err(error) => {
                    irq_state.publication_failed.store(true, Ordering::Release);
                    contain_serial_irq(core_irq, containment_cause_for_irq_wake(error), irq_state)
                }
            }
        }
        IrqCapture::Fault {
            reason,
            containment,
        } => {
            let publication = wake.publish_from_irq(
                MaintenanceCauses::IRQ,
                SerialMaintenanceEvent::Fault {
                    reason,
                    containment,
                },
            );
            match containment {
                FaultContainment::DeviceSourceMasked(_) => {
                    irq_state.action_disabled.store(true, Ordering::Release);
                    if !matches!(publication, Ok(MaintenancePublishResult::Published)) {
                        irq_state.publication_failed.store(true, Ordering::Release);
                    }
                    IrqReturn::DisableActionAndWake
                }
                FaultContainment::Uncontained => {
                    irq_state.publication_failed.store(true, Ordering::Release);
                    irq_state.line_quenched.store(true, Ordering::Release);
                    IrqReturn::MaskLineAndWake
                }
            }
        }
    }
}

fn containment_cause_for_irq_wake(error: LocalIrqWakeError) -> ax_serial::ContainmentCause {
    match error {
        LocalIrqWakeError::Closed => ax_serial::ContainmentCause::PublicationClosed,
        LocalIrqWakeError::NotHardIrq
        | LocalIrqWakeError::WrongCpu { .. }
        | LocalIrqWakeError::OwnerIdentityMismatch
        | LocalIrqWakeError::OwnerPlacementMismatch { .. }
        | LocalIrqWakeError::OwnerUnavailable { .. } => {
            ax_serial::ContainmentCause::OwnerUnavailable
        }
    }
}

fn contain_serial_irq(
    core_irq: &mut LocalOwnerIrq<SerialCore>,
    cause: ax_serial::ContainmentCause,
    irq_state: &SerialIrqState,
) -> IrqReturn {
    match core_irq.with_irq(|core| IrqEndpoint::contain(core, cause)) {
        Ok(Ok(_)) => {
            irq_state.action_disabled.store(true, Ordering::Release);
            IrqReturn::DisableActionAndWake
        }
        Ok(Err(_)) | Err(_) => {
            irq_state.line_quenched.store(true, Ordering::Release);
            IrqReturn::MaskLineAndWake
        }
    }
}

fn serial_owner_loop(
    backend: &SerialBackend,
    session: &MaintenanceSession<SerialMaintenanceEvent>,
    owner: &LocalOwnerControl<SerialCore>,
    registration: &MaintenanceIrqAction,
) -> Result<(), MaintenanceError> {
    let mut pending_masked = None;
    loop {
        if pending_masked.is_none() {
            session.wait_for_pending()?;
        }
        let mut events = [None; SERIAL_EVENT_BATCH_LIMIT];
        let mut event_count = 0;
        let drain = session.drain_owner(SERIAL_EVENT_BATCH_LIMIT, |event| {
            events[event_count] = Some(event);
            event_count += 1;
        })?;
        let causes = drain.causes();
        if causes.contains(MaintenanceCauses::SHUTDOWN) {
            return Ok(());
        }

        let mut facts = SerialIrqEvents::default();
        let mut rearm = backend.take_pending_rearm();
        let mut fault = backend
            .irq_state
            .publication_failed
            .swap(false, Ordering::AcqRel)
            || causes.contains(MaintenanceCauses::OVERFLOW);
        for event in events.into_iter().flatten() {
            match event {
                SerialMaintenanceEvent::Irq { event, masked } => {
                    fault |= event.requires_owner_service() != masked.is_some();
                    if let Some(masked) = masked {
                        fault |= !merge_masked_source(&mut pending_masked, masked);
                    }
                }
                SerialMaintenanceEvent::Fault {
                    reason,
                    containment,
                } => {
                    warn!(
                        "{} IRQ capture fault: {reason}; containment={containment:?}",
                        backend.tty_name
                    );
                    fault = true;
                }
            }
        }

        if fault {
            recover_serial_owner(backend, owner, registration)?;
            pending_masked = None;
            continue;
        }

        if backend.start_state.load(Ordering::Acquire) == START_REQUESTED {
            start_serial_owner(backend, owner, registration)?;
        }
        if !backend.started.load(Ordering::Acquire) {
            continue;
        }

        if causes.contains(MaintenanceCauses::SUBMIT) {
            match owner.with_owner(|core| core.service(SerialSoftWork::TX_KICK)) {
                Ok(Ok(service_facts)) => merge_serial_facts(&mut facts, service_facts),
                Ok(Err(reason)) => {
                    warn!("{} TX service fault: {reason}", backend.tty_name);
                    recover_serial_owner(backend, owner, registration)?;
                    pending_masked = None;
                    continue;
                }
                Err(error) => panic!("serial owner capability failed: {error}"),
            }
        }

        if let Some(source) = pending_masked.take() {
            match owner
                .with_owner(|core| core.service_masked(source))
                .expect("serial masked service must run in its owner domain")
            {
                SerialMaskedService::Complete(service_facts) => {
                    merge_serial_facts(&mut facts, service_facts);
                    fault |= !merge_masked_source(&mut rearm, source);
                }
                SerialMaskedService::Pending(service_facts) => {
                    merge_serial_facts(&mut facts, service_facts);
                    pending_masked = Some(source);
                }
                SerialMaskedService::Backpressured(service_facts) => {
                    merge_serial_facts(&mut facts, service_facts);
                    if source.bitmap().get() & u64::from(InterruptMask::TX_SPACE.bits()) != 0 {
                        pending_masked = MaskedSource::try_new(
                            source.generation().get(),
                            u64::from(InterruptMask::TX_SPACE.bits()),
                        )
                        .ok();
                    }
                }
                SerialMaskedService::Fault(reason) => {
                    warn!("{} masked serial service fault: {reason}", backend.tty_name);
                    fault = true;
                }
                SerialMaskedService::Stale => {}
            }
        }

        publish_serial_facts(backend, facts);
        if fault {
            recover_serial_owner(backend, owner, registration)?;
            pending_masked = None;
            continue;
        }
        if let Some(source) = rearm
            && let Err(error) = owner
                .with_owner(|core| IrqSourceControl::rearm(core, source))
                .expect("serial rearm must run in its owner domain")
        {
            warn!(
                "{} failed to rearm serial source: {error}",
                backend.tty_name
            );
            recover_serial_owner(backend, owner, registration)?;
            pending_masked = None;
            continue;
        }

        apply_pending_serial_config(backend, owner);
        service_serial_drain_request(backend, owner);
        if drain.pending() || pending_masked.is_some() {
            crate::task::yield_now();
        }
    }
}

fn start_serial_owner(
    backend: &SerialBackend,
    owner: &LocalOwnerControl<SerialCore>,
    registration: &MaintenanceIrqAction,
) -> Result<(), MaintenanceError> {
    let mode = match backend.start_policy.mode() {
        Ok(mode) => mode,
        Err(_) => {
            complete_start(backend, START_FAILED);
            return Ok(());
        }
    };
    let startup = owner.with_owner(|core| {
        let config = match mode.startup_baudrate(|| core.baudrate()) {
            Some(baudrate) => Config::new().baudrate(baudrate),
            None => Config::new(),
        };
        core.startup(&config)
    });
    if !matches!(startup, Ok(Ok(()))) {
        let recovered = recover_failed_start(mode, owner);
        complete_start(
            backend,
            if recovered {
                START_FAILED
            } else {
                START_RECOVERY_FAILED
            },
        );
        return Ok(());
    }
    if registration.enable().is_err() {
        let _ = registration.disable();
        let _ = registration.synchronize();
        let recovered = recover_failed_start(mode, owner);
        complete_start(
            backend,
            if recovered {
                START_FAILED
            } else {
                START_RECOVERY_FAILED
            },
        );
        return Ok(());
    }
    if backend.irq_state.line_quenched.load(Ordering::Acquire) {
        if registration.release_quench().is_err() {
            recover_serial_owner(backend, owner, registration)?;
            complete_start(backend, START_RECOVERY_FAILED);
            return Ok(());
        }
        backend
            .irq_state
            .line_quenched
            .store(false, Ordering::Release);
    }
    let activated = owner.with_owner(SerialCore::activate_interrupts);
    if !matches!(activated, Ok(Ok(()))) {
        recover_serial_owner(backend, owner, registration)?;
        complete_start(backend, START_RECOVERY_FAILED);
        return Ok(());
    }
    let _ = backend
        .irq_state
        .action_disabled
        .swap(false, Ordering::AcqRel);
    backend.started.store(true, Ordering::Release);
    complete_start(backend, START_SUCCEEDED);
    Ok(())
}

fn recover_failed_start(mode: SerialStartMode, owner: &LocalOwnerControl<SerialCore>) -> bool {
    match mode.failed_start_recovery() {
        FailedStartRecovery::RestoreBootPolling => {
            owner.with_owner(SerialCore::quiesce_to_polling).is_ok()
        }
        FailedStartRecovery::ShutdownPort => owner.with_owner(SerialCore::shutdown).is_ok(),
    }
}

fn complete_start(backend: &SerialBackend, state: u8) {
    backend.start_state.store(state, Ordering::Release);
    backend.start_wait.notify_all();
    if state == START_SUCCEEDED && backend.start_policy.mode() == Ok(SerialStartMode::ConfigurePort)
    {
        backend.publish_started_events();
    }
}

fn recover_serial_owner(
    backend: &SerialBackend,
    owner: &LocalOwnerControl<SerialCore>,
    registration: &MaintenanceIrqAction,
) -> Result<(), MaintenanceError> {
    let _ = backend.take_pending_rearm();
    quiesce_serial_irq(backend, owner, registration)?;
    backend.started.store(false, Ordering::Release);
    if backend.start_state.load(Ordering::Acquire) == START_REQUESTED {
        complete_start(backend, START_FAILED);
    }
    backend.tx_progress.fetch_add(1, Ordering::AcqRel);
    backend.tx_wait.notify_all();
    unsafe {
        backend
            .input_source
            .wake(IoEvents::IN | IoEvents::ERR | IoEvents::HUP);
        backend
            .output_source
            .wake(IoEvents::OUT | IoEvents::ERR | IoEvents::HUP);
    }
    Ok(())
}

fn quiesce_serial_irq(
    backend: &SerialBackend,
    owner: &LocalOwnerControl<SerialCore>,
    registration: &MaintenanceIrqAction,
) -> Result<(), MaintenanceError> {
    owner
        .with_owner(SerialCore::shutdown)
        .expect("serial quiesce must run in its owner domain");
    registration.disable()?;
    if backend.irq_state.line_quenched.load(Ordering::Acquire) {
        registration.release_quench()?;
        backend
            .irq_state
            .line_quenched
            .store(false, Ordering::Release);
    }
    registration.synchronize()?;
    Ok(())
}

fn apply_pending_serial_config(backend: &SerialBackend, owner: &LocalOwnerControl<SerialCore>) {
    let Some(config) = backend.pending_config.lock().take() else {
        return;
    };
    match owner.with_owner(|core| core.set_config(&config)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(
            "{} failed to apply serial configuration: {error}",
            backend.tty_name
        ),
        Err(error) => panic!("serial configuration escaped its owner domain: {error}"),
    }
}

fn service_serial_drain_request(backend: &SerialBackend, owner: &LocalOwnerControl<SerialCore>) {
    if !backend.drain_requested.swap(false, Ordering::AcqRel) {
        return;
    }
    let idle = owner
        .with_owner(SerialCore::tx_idle)
        .expect("serial drain check must run in its owner domain");
    backend.drain_complete.store(idle, Ordering::Release);
    backend.tx_progress.fetch_add(1, Ordering::AcqRel);
    backend.tx_wait.notify_all();
}

fn publish_serial_facts(backend: &SerialBackend, facts: SerialIrqEvents) {
    if facts.rx_pushed > 0 {
        unsafe { backend.input_source.wake(IoEvents::IN) };
    }
    if facts.tx_sent > 0 || facts.tx_wakeup {
        backend.tx_progress.fetch_add(1, Ordering::AcqRel);
        backend.tx_wait.notify_all();
        unsafe { backend.output_source.wake(IoEvents::OUT) };
    }
}

fn merge_serial_facts(target: &mut SerialIrqEvents, facts: SerialIrqEvents) {
    target.rx_pushed = target.rx_pushed.saturating_add(facts.rx_pushed);
    target.tx_sent = target.tx_sent.saturating_add(facts.tx_sent);
    target.tx_wakeup |= facts.tx_wakeup;
}

fn merge_masked_source(target: &mut Option<MaskedSource>, source: MaskedSource) -> bool {
    let Some(current) = *target else {
        *target = Some(source);
        return true;
    };
    if current.generation() != source.generation() {
        return false;
    }
    let bitmap = current.bitmap().get() | source.bitmap().get();
    *target = MaskedSource::try_new(current.generation().get(), bitmap).ok();
    target.is_some()
}

fn close_serial_maintenance(
    backend: &SerialBackend,
    session: MaintenanceSession<SerialMaintenanceEvent>,
    owner_cell: core::pin::Pin<alloc::boxed::Box<LocalOwnerCell<SerialCore>>>,
    owner: LocalOwnerControl<SerialCore>,
    registration: Option<MaintenanceIrqAction>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    backend.close_serial_admission();
    let begin_close = session.begin_close();
    if let Some(registration) = registration {
        if let Err(error) = quiesce_serial_irq(backend, &owner, &registration) {
            warn!("{} failed to quiesce serial IRQ: {error}", backend.tty_name);
            let _retained_owner = (owner_cell, owner, registration);
            session.quarantine_and_park();
        }
        if let Err(failure) = registration.close() {
            let (reason, registration) = failure.into_parts();
            warn!(
                "{} failed to destroy serial IRQ action: {reason:?}",
                backend.tty_name
            );
            let _retained_owner = (owner_cell, owner, registration);
            session.quarantine_and_park();
        }
    } else {
        owner
            .with_owner(SerialCore::shutdown)
            .expect("serial close must retain owner access");
    }
    begin_close?;
    while session.state() == MaintenanceState::Closing {
        let drain = session.drain_owner(SERIAL_EVENT_BATCH_LIMIT, |_| {})?;
        if !drain.pending() {
            break;
        }
    }
    session.try_begin_draining()?;
    session.finish_close()?;
    let closed = session
        .try_into_closed()
        .map_err(|failure| failure.error())?;
    owner_cell
        .reclaim(owner, &closed)
        .unwrap_or_else(|failure| {
            panic!("failed to reclaim closed serial owner: {}", failure.error())
        });
    Ok(closed)
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
            let observed = self.backend.tx_progress.load(Ordering::Acquire);
            let count = self.backend.submit_tx(&buf[written..]);
            if count == 0 {
                self.backend.tx_wait.wait_until(|| {
                    self.backend.tx_progress.load(Ordering::Acquire) != observed
                        || !self.backend.started.load(Ordering::Acquire)
                });
                if !self.backend.started.load(Ordering::Acquire) {
                    return;
                }
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
        self.backend.submit_tx(buf)
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
        ConsoleCandidate, ConsoleDeviceIdError, ConsoleSelection, EmergencyFlushResult,
        EmergencyWriteResult, assign_tty_numbers, emergency_flush_outcome, emergency_write_outcome,
        select_console_candidate,
    };

    #[test]
    fn emergency_output_without_owner_fails_fast_without_claiming_progress() {
        assert_eq!(
            emergency_write_outcome(true, false),
            EmergencyWriteResult::Busy
        );
        assert_eq!(
            emergency_write_outcome(false, false),
            EmergencyWriteResult::Fault
        );
        assert_eq!(
            emergency_write_outcome(true, true),
            EmergencyWriteResult::Written { count: 0 }
        );
        assert_eq!(emergency_flush_outcome(true), EmergencyFlushResult::Busy);
        assert_eq!(emergency_flush_outcome(false), EmergencyFlushResult::Fault);
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
