use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_driver::serial::{
    self as ax_serial, Config, RxFlag, RxItem, SerialDevice, SerialIrqOutcome, SerialPort,
    SerialSoftWork,
};
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::{
    console::{ConsoleDeviceIdError, ConsoleDeviceIdResult},
    irq::{AutoEnable, CpuId, IrqAffinity, IrqExecution, IrqHandle, IrqRequest, ShareMode},
};
use ax_sync::Mutex;
use ax_task::IrqNotify;
use axpoll::{IoEvents, PollSet};
use bitflags::bitflags;
use rdrive::DeviceId as RDriveDeviceId;
use spin::LazyLock;
use starry_process::Process;

use super::{
    Tty,
    terminal::{
        Terminal,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
        termios::Termios2,
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
    port: SerialPort,
    irq_num: usize,
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
    pending: AtomicU32,
    notify: IrqNotify,
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
                .map(|serial| serial.alias_index())
                .collect::<Vec<_>>()
                .as_slice(),
        );

        let mut entries = Vec::new();
        for (serial, number) in serials.into_iter().zip(numbers) {
            let Some(number) = number else {
                warn!(
                    "Skipping serial device {} at {} because ttyS number could not be assigned",
                    serial.name(),
                    serial.fdt_path()
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
    let name = serial.name().into();
    let info = serial.info().clone();
    let rdrive_device_id = serial.rdrive_device_id();
    let Some(irq_num) = serial.irq_num() else {
        return Err(AxError::Unsupported);
    };
    let port = serial.into_port();
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        rdrive_device_id,
        number,
        port,
        irq_num,
        irq_handle: SpinNoIrq::new(None),
        started: AtomicBool::new(false),
        start_lock: Mutex::new(()),
        events: SerialEvents::new(),
        input_source: Arc::new(PollSet::new()),
        output_source: Arc::new(PollSet::new()),
        tx_notify: IrqNotify::new(),
        output_lock: Mutex::new(()),
    });

    backend.register_irq()?;
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
        tty_name, info.fdt_path, info.alias_index, info.paddr, info.mapped_base, irq_num
    );
    Ok(SerialTtyEntry {
        number,
        tty,
        backend: entry_backend,
    })
}

impl SerialBackend {
    fn register_irq(self: &Arc<Self>) -> AxResult<()> {
        let data = NonNull::new(Arc::into_raw(self.clone()) as *mut ()).unwrap();
        let request = IrqRequest::new(serial_raw_irq_handler, data)
            .share_mode(ShareMode::Shared)
            .affinity(IrqAffinity::Fixed(CpuId(self.port.owner_cpu())))
            .execution(IrqExecution::NonReentrant)
            .auto_enable(AutoEnable::No);
        match ax_runtime::hal::irq::request_irq(self.irq_num, request) {
            Ok(handle) => {
                *self.irq_handle.lock() = Some(handle);
                Ok(())
            }
            Err(err) => {
                unsafe {
                    Arc::decrement_strong_count(data.as_ptr() as *const SerialBackend);
                }
                warn!(
                    "Failed to register {} IRQ handler for irq {}: {err:?}",
                    self.tty_name, self.irq_num
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

        if let Err(err) = self
            .port
            .startup(&Config::new().baudrate(startup_baudrate(self.port.baudrate())))
        {
            warn!(
                "{} failed to start serial port {}: {:?}",
                self.tty_name, self.name, err
            );
            return false;
        }

        if let Err(err) = ax_runtime::hal::irq::enable_irq(handle) {
            let _ = self.port.shutdown();
            warn!(
                "Failed to enable {} IRQ handler for irq {}: {err:?}",
                self.tty_name, self.irq_num
            );
            return false;
        }

        self.started.store(true, Ordering::Release);
        publish_serial_outcome(
            self,
            self.port.service_on_owner(SerialSoftWork::RESERVICE),
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
}

fn startup_baudrate(current: u32) -> u32 {
    if current == 0 {
        SERIAL_DEFAULT_BAUDRATE
    } else {
        current
    }
}

fn spawn_serial_event_worker(backend: Arc<SerialBackend>) {
    let task_name = format!("{}-event", backend.tty_name);
    ax_task::spawn_with_name(
        move || loop {
            backend.events.wait();
            loop {
                let pending = backend.events.take();
                if pending.is_empty() {
                    break;
                }
                if pending.contains(SerialEventBits::RX_READY) {
                    unsafe { backend.input_source.wake(IoEvents::IN) };
                }
                if pending.contains(SerialEventBits::TX_SPACE) {
                    backend.tx_notify.notify();
                    unsafe { backend.output_source.wake(IoEvents::OUT) };
                    let outcome = backend.port.service_on_owner(SerialSoftWork::TX_KICK);
                    publish_serial_outcome(&backend, outcome, false);
                }
            }
        },
        task_name,
    );
}

unsafe fn serial_raw_irq_handler(
    ctx: ax_runtime::hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    let backend = unsafe { &*(data.as_ptr() as *const SerialBackend) };
    let outcome = backend.port.handle_irq_on_owner(ctx.cpu);
    if !outcome.claimed {
        return ax_runtime::hal::irq::IrqReturn::Unhandled;
    }
    let events = publish_serial_outcome(backend, outcome, true);
    if events.is_empty() {
        ax_runtime::hal::irq::IrqReturn::Handled
    } else {
        ax_runtime::hal::irq::IrqReturn::Wake
    }
}

fn publish_serial_outcome(
    backend: &SerialBackend,
    outcome: SerialIrqOutcome,
    from_irq: bool,
) -> SerialEventBits {
    let mut events = SerialEventBits::empty();
    if outcome.rx_pushed > 0 {
        events |= SerialEventBits::RX_READY;
    }
    if outcome.tx_wakeup {
        events |= SerialEventBits::TX_SPACE;
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
            let read = self.backend.port.drain_rx(&mut temp[..limit]);
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
            let (count, outcome) = self.backend.port.submit_tx(&buf[written..]);
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
        let (count, outcome) = self.backend.port.submit_tx(buf);
        publish_serial_outcome(&self.backend, outcome, false);
        count
    }

    fn flush_echo_before_input(&self) -> bool {
        true
    }

    fn max_sync_echo_bytes(&self) -> usize {
        SERIAL_SYNC_ECHO_LIMIT
    }

    fn termios_changed(&self, old: &Termios2, new: &Termios2) {
        let Some(new_baud) = new.baudrate() else {
            return;
        };
        if old.baudrate() == Some(new_baud) {
            return;
        }
        if self.backend.ensure_started().is_err() {
            return;
        }
        if let Err(err) = self
            .backend
            .port
            .set_config(&Config::new().baudrate(new_baud))
        {
            warn!(
                "{} failed to set baudrate {new_baud} on {}: {:?}",
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
