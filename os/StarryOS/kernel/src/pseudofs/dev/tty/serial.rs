use alloc::{collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use ax_driver::serial::{
    self as ax_serial, BIrqHandler, BRxQueue, BTxQueue, SerialDevice, SerialEvent,
    SerialRuntimePortControl,
};
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::irq::{AutoEnable, IrqHandle, IrqRequest, ShareMode};
use ax_task::IrqNotify;
use axpoll::{IoEvents, PollSet};
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

const SERIAL_RX_BUFFER_CAP: usize = 4096;
const SERIAL_RX_DRAIN_CHUNK: usize = 64;
const SERIAL_IRQ_KICK_INTERVAL: Duration = Duration::from_millis(10);
const SERIAL_RX_EVENT_MASK: SerialEvent = SerialEvent::RX_READY
    .union(SerialEvent::RX_ERROR)
    .union(SerialEvent::OVERRUN);

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
    control: SpinNoIrq<SerialRuntimePortControl>,
    tx: SpinNoIrq<BTxQueue>,
    rx: SpinNoIrq<BRxQueue>,
    rx_buffer: SpinNoIrq<VecDeque<u8>>,
    irq_handler: Option<BIrqHandler>,
    irq_num: Option<usize>,
    irq_handle: SpinNoIrq<Option<IrqHandle>>,
    irq_armed: AtomicBool,
    irq_state_kick_required: bool,
    input_source: Arc<PollSet>,
    input_notify: IrqNotify,
    tx_notify: IrqNotify,
    rx_dropped: AtomicUsize,
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
        return entry.tty.bind_to(proc);
    }
    Err(AxError::NoSuchDevice)
}

pub fn arm_console_irq() {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        entry.backend.arm_irq();
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
    let input_source = Arc::new(PollSet::new());
    let runtime = serial.into_runtime_port()?;
    let name = runtime.name().into();
    let info = runtime.info().clone();
    let rdrive_device_id = runtime.rdrive_device_id();
    let irq_num = runtime.irq_num();
    let irq_state_kick_required = info.rx_polling_required;
    let (control, tx, rx, irq_handler) = runtime.split();
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        rdrive_device_id,
        number,
        control: SpinNoIrq::new(control),
        tx: SpinNoIrq::new(tx),
        rx: SpinNoIrq::new(rx),
        rx_buffer: SpinNoIrq::new(VecDeque::with_capacity(SERIAL_RX_BUFFER_CAP)),
        irq_handler,
        irq_num,
        irq_handle: SpinNoIrq::new(None),
        irq_armed: AtomicBool::new(false),
        irq_state_kick_required,
        input_source,
        input_notify: IrqNotify::new(),
        tx_notify: IrqNotify::new(),
        rx_dropped: AtomicUsize::new(0),
    });
    let process_mode = serial_process_mode(&backend)?;
    let mode_name = process_mode_name(&process_mode);
    let terminal = Arc::new(Terminal::default());
    let entry_backend = backend.clone();
    let tty = Tty::new(
        terminal,
        TtyConfig {
            reader: SerialReader {
                backend: backend.clone(),
            },
            writer: SerialWriter { backend },
            process_mode,
        },
    );
    info!(
        "{} registered: path={}, alias={:?}, paddr={:#x}, mapped={:#x}, irq={:?}, mode={}",
        tty_name, info.fdt_path, info.alias_index, info.paddr, info.mapped_base, irq_num, mode_name
    );
    Ok(SerialTtyEntry {
        number,
        tty,
        backend: entry_backend,
    })
}

fn serial_process_mode(backend: &Arc<SerialBackend>) -> AxResult<ProcessMode> {
    let Some(irq_num) = backend.irq_num else {
        warn!(
            "{} has no IRQ; Starry serial tty requires interrupt mode",
            backend.tty_name
        );
        return Err(ax_errno::AxError::Unsupported);
    };
    if backend.irq_handler.is_none() {
        warn!(
            "{} has irq {irq_num} but no serial IRQ handler; Starry serial tty requires interrupt \
             mode",
            backend.tty_name
        );
        return Err(ax_errno::AxError::Unsupported);
    }

    let data = NonNull::new(Arc::into_raw(backend.clone()) as *mut ()).unwrap();
    let request = IrqRequest::new(serial_raw_irq_handler, data)
        .share_mode(ShareMode::Shared)
        .auto_enable(AutoEnable::No);
    let handle = match ax_runtime::hal::irq::request_irq(irq_num, request) {
        Ok(handle) => handle,
        Err(err) => {
            warn!(
                "Failed to register {} IRQ handler for irq {irq_num}: {err:?}",
                backend.tty_name,
            );
            unsafe {
                Arc::decrement_strong_count(data.as_ptr() as *const SerialBackend);
            }
            return Err(ax_errno::AxError::Unsupported);
        }
    };
    *backend.irq_handle.lock() = Some(handle);
    spawn_serial_irq_drain(backend.clone());
    if backend.irq_state_kick_required {
        spawn_serial_irq_state_kick(backend.clone());
    }
    Ok(ProcessMode::InterruptDriven(backend.input_source.clone()))
}

fn process_mode_name(mode: &ProcessMode) -> &'static str {
    match mode {
        ProcessMode::InterruptDriven(_) => "interrupt",
        ProcessMode::Passive(_) => "passive",
    }
}

fn spawn_serial_irq_drain(backend: Arc<SerialBackend>) {
    let task_name = format!("{}-irq-drain", backend.tty_name);
    ax_task::spawn_with_name(
        move || loop {
            backend.input_notify.wait();
            let had_data = backend.has_rx_buffered_data();
            let drained = backend.drain_rx_to_buffer(true);
            let dropped = backend.rx_dropped.swap(0, Ordering::AcqRel);
            if dropped > 0 {
                warn!(
                    "{} software RX buffer full; dropped {dropped} byte(s)",
                    backend.tty_name
                );
            }
            if had_data || drained > 0 {
                // The serial IRQ worker runs in task context; hard IRQ only
                // publishes `input_notify`, so this poll wake never runs from
                // the IRQ callback.
                unsafe { backend.input_source.wake(IoEvents::IN) };
            }
        },
        task_name,
    );
}

fn spawn_serial_irq_state_kick(backend: Arc<SerialBackend>) {
    let task_name = format!("{}-irq-state-kick", backend.tty_name);
    ax_task::spawn_with_name(
        move || loop {
            if backend.irq_armed.load(Ordering::Acquire) {
                // Some UARTs expose an IRQ line but do not reliably deliver RX
                // readiness to the kernel. Keep this below the tty layer: the
                // kicker only runs the same IRQ endpoint as hard IRQ, then the
                // normal IrqNotify workers drain queues and wake poll waiters.
                // TX/RX queues still never poll raw hardware status directly.
                backend.sync_irq_events();
            }
            ax_task::sleep(SERIAL_IRQ_KICK_INTERVAL);
        },
        task_name,
    );
}

impl SerialBackend {
    fn notify_rx_drain(&self) {
        self.input_notify.notify();
    }

    fn sync_irq_events(&self) -> SerialEvent {
        let status = self
            .irq_handler
            .as_ref()
            .map(|handler| handler.handle_irq())
            .unwrap_or_else(SerialEvent::empty);
        self.notify_events(status);
        status
    }

    fn notify_events(&self, status: SerialEvent) {
        if status.intersects(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN) {
            self.input_notify.notify();
        }
        if status.intersects(SerialEvent::TX_READY | SerialEvent::TX_ERROR) {
            self.tx_notify.notify();
        }
    }

    fn notify_events_from_irq(&self, status: SerialEvent) {
        if status.intersects(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN) {
            self.input_notify.notify_irq();
        }
        if status.intersects(SerialEvent::TX_READY | SerialEvent::TX_ERROR) {
            self.tx_notify.notify_irq();
        }
    }

    fn arm_irq(&self) -> bool {
        let Some(irq_num) = self.irq_num else {
            return false;
        };
        let Some(handle) = *self.irq_handle.lock() else {
            return false;
        };
        if self.irq_armed.swap(true, Ordering::AcqRel) {
            return true;
        }

        self.control.lock().enable_rx_interrupts();
        if let Err(err) = ax_runtime::hal::irq::enable_irq(handle) {
            self.control.lock().disable_rx_interrupts();
            self.irq_armed.store(false, Ordering::Release);
            warn!(
                "Failed to arm {} IRQ handler for irq {irq_num}: {err:?}",
                self.tty_name
            );
            false
        } else {
            info!("{} IRQ armed on irq {irq_num}", self.tty_name);
            true
        }
    }

    fn pop_rx_buffer(&self, buf: &mut [u8]) -> usize {
        let mut queue = self.rx_buffer.lock();
        let read = buf.len().min(queue.len());
        for slot in buf.iter_mut().take(read) {
            *slot = queue.pop_front().unwrap();
        }
        read
    }

    fn has_rx_buffered_data(&self) -> bool {
        !self.rx_buffer.lock().is_empty()
    }

    fn push_rx_buffer(&self, bytes: &[u8]) -> usize {
        let mut queue = self.rx_buffer.lock();
        let mut queued = 0;
        for &byte in bytes {
            if queue.len() == SERIAL_RX_BUFFER_CAP {
                break;
            }
            queue.push_back(byte);
            queued += 1;
        }
        if queued < bytes.len() {
            self.rx_dropped
                .fetch_add(bytes.len() - queued, Ordering::AcqRel);
        }
        queued
    }

    fn read_hardware_rx(&self, buf: &mut [u8], log_errors: bool) -> usize {
        match self.rx.lock().try_read(buf) {
            Ok(read) => read,
            Err(err) => {
                if log_errors {
                    if err.bytes_transferred == 0 {
                        warn!(
                            "{} read error from {}: {:?}",
                            self.tty_name, self.name, err.kind
                        );
                    } else {
                        warn!(
                            "{} read error from {} after preserving {} byte(s): {:?}",
                            self.tty_name, self.name, err.bytes_transferred, err.kind
                        );
                    }
                }
                err.bytes_transferred
            }
        }
    }

    fn drain_rx_to_buffer(&self, log_errors: bool) -> usize {
        let mut total = 0;
        loop {
            let space = {
                let queue = self.rx_buffer.lock();
                SERIAL_RX_BUFFER_CAP.saturating_sub(queue.len())
            };
            if space == 0 {
                break;
            }

            let mut chunk = [0; SERIAL_RX_DRAIN_CHUNK];
            let limit = chunk.len().min(space);
            let read = self.read_hardware_rx(&mut chunk[..limit], log_errors);
            if read == 0 {
                break;
            }

            let queued = self.push_rx_buffer(&chunk[..read]);
            total += queued;
            if queued < read {
                break;
            }

            // RX queues are IRQ-state driven and never poll hardware directly.
            // After consuming one saved RX snapshot, resample through the same
            // serial IRQ endpoint so a FIFO burst can be drained immediately
            // instead of waiting for the fallback state kicker.
            if !self.sync_irq_events().intersects(SERIAL_RX_EVENT_MASK) {
                break;
            }
        }
        total
    }
}

unsafe fn serial_raw_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    let backend = unsafe { &*(data.as_ptr() as *const SerialBackend) };
    if let Some(handler) = backend.irq_handler.as_ref() {
        let status = handler.handle_irq();
        backend.notify_events_from_irq(status);
    }
    ax_runtime::hal::irq::IrqReturn::Handled
}

impl TtyRead for SerialReader {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        let read = self.backend.pop_rx_buffer(buf);
        if read > 0 {
            self.backend.notify_rx_drain();
            return read;
        }

        if self.backend.arm_irq() {
            self.backend.notify_rx_drain();
        }
        0
    }
}

impl TtyWrite for SerialWriter {
    fn write(&self, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        if !self.backend.arm_irq() {
            return;
        }

        self.backend.control.lock().enable_tx_interrupts();
        let mut written = 0;
        while written < buf.len() {
            let mut tx = self.backend.tx.lock();
            let next = tx.try_write(&buf[written..]);
            written += next;
            if next == 0 {
                let status = tx.poll();
                if status.intersects(SerialEvent::TX_ERROR) {
                    warn!(
                        "{} TX error from {}",
                        self.backend.tty_name, self.backend.name
                    );
                    break;
                }
                if status.intersects(SerialEvent::TX_READY) {
                    drop(tx);
                    ax_task::yield_now();
                    continue;
                }
                drop(tx);
                self.backend.tx_notify.wait();
            }
        }
        self.backend.control.lock().disable_tx_interrupts();
    }

    fn termios_changed(&self, old: &Termios2, new: &Termios2) {
        let Some(new_baud) = new.baudrate() else {
            return;
        };
        if old.baudrate() == Some(new_baud) {
            return;
        }
        let _tx = self.backend.tx.lock();
        let _rx = self.backend.rx.lock();
        if let Err(err) = self.backend.control.lock().set_baudrate(new_baud) {
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
    selected_device_id: Option<RDriveDeviceId>,
) -> Option<ConsoleSelection> {
    if let Some(device_id) = selected_device_id {
        if let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.device_id == device_id)
        {
            return Some(ConsoleSelection::SelectedDevice(index));
        }
        warn!(
            "selected console device {device_id:?} did not match a discovered serial TTY; trying \
             ttyS0"
        );
    }

    candidates
        .iter()
        .position(|candidate| candidate.number == 0)
        .map(ConsoleSelection::TtyS0Fallback)
}

#[cfg(test)]
mod tests {
    use rdrive::DeviceId as RDriveDeviceId;

    use super::{ConsoleCandidate, ConsoleSelection, assign_tty_numbers, select_console_candidate};

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
            select_console_candidate(&candidates, Some(tty_s1)),
            Some(ConsoleSelection::SelectedDevice(1))
        );
    }

    #[test]
    fn unmatched_device_id_falls_back_to_ttys0() {
        let tty_s0 = RDriveDeviceId::from(10);
        let missing = RDriveDeviceId::from(99);
        let candidates = [ConsoleCandidate {
            number: 0,
            device_id: tty_s0,
        }];

        assert_eq!(
            select_console_candidate(&candidates, Some(missing)),
            Some(ConsoleSelection::TtyS0Fallback(0))
        );
    }

    #[test]
    fn missing_device_id_falls_back_to_ttys0() {
        let tty_s0 = RDriveDeviceId::from(10);
        let candidates = [ConsoleCandidate {
            number: 0,
            device_id: tty_s0,
        }];

        assert_eq!(
            select_console_candidate(&candidates, None),
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

        assert_eq!(select_console_candidate(&candidates, None), None);
    }
}
