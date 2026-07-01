use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_driver::serial::{
    self as ax_serial, Config, ConfigError, DataBits, OwnerId, Parity, RxFlag, RxItem, RxQueue,
    SerialDevice, SerialIrqHandler, SerialIrqOutcome, SerialPort, SerialSoftWork, StopBits,
    TxQueue,
};
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::{
    console::{ConsoleDeviceIdError, ConsoleDeviceIdResult},
    irq::{AutoEnable, CpuId, IrqAffinity, IrqHandle, IrqId, IrqRequest, ShareMode},
};
use ax_sync::Mutex;
use ax_task::{IrqNotify, IrqTaskWaker, local::RuntimeEvent};
use axpoll::{IoEvents, PollSet};
use bitflags::bitflags;
use rdrive::DeviceId as RDriveDeviceId;
use spin::{LazyLock, Once};
use starry_process::Process;

use super::{
    Tty,
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
const SERIAL_DEFAULT_BAUDRATE: u32 = 115_200;

bitflags! {
    #[derive(Clone, Copy, Debug, Default)]
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
    started: AtomicBool,
    start_lock: Mutex<()>,
    events: SerialEvents,
    input_source: Arc<PollSet>,
    output_source: Arc<PollSet>,
    tx_notify: IrqNotify,
    output_lock: Mutex<()>,
}

struct SerialEvents {
    event: RuntimeEvent,
    irq_waker: Once<IrqTaskWaker>,
    pending_hint: AtomicBool,
    rx_seq: AtomicU64,
    tx_seq: AtomicU64,
}

impl SerialEvents {
    const fn new() -> Self {
        Self {
            event: RuntimeEvent::new(),
            irq_waker: Once::new(),
            pending_hint: AtomicBool::new(false),
            rx_seq: AtomicU64::new(0),
            tx_seq: AtomicU64::new(0),
        }
    }

    fn init_irq_waker(&self, waker: IrqTaskWaker) {
        self.irq_waker.call_once(|| waker);
    }

    fn publish_irq(&self, events: SerialEventBits) {
        if events.is_empty() {
            return;
        }
        self.pending_hint.store(true, Ordering::Release);
        let bits = u64::from(events.bits());
        if let Some(waker) = self.irq_waker.get() {
            let _ = self.event.publish_from_irq_with(bits, waker);
        } else {
            let _ = self.event.publish_from_irq(bits);
        }
    }

    fn publish(&self, events: SerialEventBits) {
        if events.is_empty() {
            return;
        }
        self.pending_hint.store(true, Ordering::Release);
        self.event.publish(u64::from(events.bits()));
    }

    fn wait(&self) {
        self.event
            .wait_until(|| self.pending_hint.load(Ordering::Acquire));
    }

    fn take(&self) -> SerialEventBits {
        self.pending_hint.store(false, Ordering::Release);
        let bits = self.event.take_bits();
        SerialEventBits::from_bits_retain(bits as u32)
    }

    fn wake_waiters_deferred(&self) {
        self.event.wake_waiters_deferred();
    }

    fn observe_rx_seq(&self, seq: u64) -> bool {
        let mut current = self.rx_seq.load(Ordering::Acquire);
        while seq > current {
            match self.rx_seq.compare_exchange_weak(
                current,
                seq,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
        false
    }

    fn observe_tx_seq(&self, seq: u64) -> bool {
        let mut current = self.tx_seq.load(Ordering::Acquire);
        while seq > current {
            match self.tx_seq.compare_exchange_weak(
                current,
                seq,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
        false
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

pub fn bind_console_to(proc: &Process) -> AxResult<()> {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        entry.backend.ensure_started()?;
        ax_runtime::hal::console::claim_runtime_output();
        return entry.tty.bind_to(proc);
    }
    Err(AxError::NoSuchDevice)
}

pub fn arm_console_irq() {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        entry.backend.start_port();
    }
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
        started: AtomicBool::new(false),
        start_lock: Mutex::new(()),
        events: SerialEvents::new(),
        input_source: Arc::new(PollSet::new()),
        output_source: Arc::new(PollSet::new()),
        tx_notify: IrqNotify::new(),
        output_lock: Mutex::new(()),
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

    fn start_port(&self) -> bool {
        if self.started.load(Ordering::Acquire) {
            return true;
        }
        let _guard = self.start_lock.lock();
        if self.started.load(Ordering::Acquire) {
            return true;
        }

        let Some(handle) = *self.irq_handle.lock() else {
            return false;
        };

        if let Err(err) =
            self.startup_port(&Config::new().baudrate(startup_baudrate(self.baudrate())))
        {
            warn!(
                "{} failed to start serial port {}: {:?}",
                self.tty_name, self.name, err
            );
            return false;
        }

        if let Err(err) = ax_runtime::hal::irq::enable_irq(handle) {
            self.shutdown_port();
            warn!(
                "Failed to enable {} IRQ handler for irq {:?}: {err:?}",
                self.tty_name, self.irq
            );
            return false;
        }

        self.started.store(true, Ordering::Release);
        publish_serial_outcome(
            self,
            self.service_on_owner(SerialSoftWork::RESERVICE),
            false,
        );
        self.events.publish(SerialEventBits::RX_READY);
        true
    }

    fn ensure_started(&self) -> AxResult<()> {
        if self.start_port() {
            Ok(())
        } else {
            Err(AxError::Unsupported)
        }
    }

    fn startup_port(&self, config: &Config) -> Result<SerialIrqOutcome, ConfigError> {
        ax_serial::run_on_owner(self.owner, |lease| self.port.startup(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    fn shutdown_port(&self) {
        let _ = ax_serial::run_on_owner(self.owner, |lease| self.port.shutdown(lease));
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
            ax_task::sleep(Duration::from_millis(1));
        }
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.rx.lock().drain(out)
    }
}

fn startup_baudrate(current: u32) -> u32 {
    if current == 0 {
        SERIAL_DEFAULT_BAUDRATE
    } else {
        current
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
    ax_task::spawn_with_name(
        move || {
            backend
                .events
                .init_irq_waker(ax_task::current_irq_task_waker());
            loop {
                backend.events.wait();
                loop {
                    let pending = backend.events.take();
                    if pending.is_empty() {
                        break;
                    }
                    backend.events.wake_waiters_deferred();
                    if pending.contains(SerialEventBits::RX_READY) {
                        unsafe { backend.input_source.wake(IoEvents::IN) };
                    }
                    if pending.contains(SerialEventBits::TX_SPACE) {
                        backend.tx_notify.notify();
                        unsafe { backend.output_source.wake(IoEvents::OUT) };
                        let outcome = backend.service_on_owner(SerialSoftWork::TX_KICK);
                        publish_serial_outcome(&backend, outcome, false);
                    }
                    if pending.contains(SerialEventBits::RESERVICE) {
                        let outcome = backend.service_on_owner(SerialSoftWork::RESERVICE);
                        publish_serial_outcome(&backend, outcome, false);
                    }
                }
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
    let mut events = SerialEventBits::empty();
    if outcome.rx_pushed > 0 || backend.events.observe_rx_seq(outcome.snapshot.rx_seq) {
        events |= SerialEventBits::RX_READY;
    }
    if outcome.tx_wakeup || backend.events.observe_tx_seq(outcome.snapshot.tx_seq) {
        events |= SerialEventBits::TX_SPACE;
    }
    if outcome.snapshot.hangup {
        events |= SerialEventBits::HANGUP;
    }
    if outcome.budget_exhausted || outcome.snapshot.service_needed {
        events |= SerialEventBits::RESERVICE;
    }

    if from_irq {
        backend.events.publish_irq(events);
    } else {
        backend.events.publish(events);
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
        self.backend.ensure_started()
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
        ConsoleCandidate, ConsoleDeviceIdError, ConsoleSelection, assign_tty_numbers,
        select_console_candidate,
    };

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

    #[test]
    fn zero_hardware_baudrate_uses_runtime_default() {
        assert_eq!(super::startup_baudrate(0), super::SERIAL_DEFAULT_BAUDRATE);
        assert_eq!(super::startup_baudrate(1_500_000), 1_500_000);
    }
}
