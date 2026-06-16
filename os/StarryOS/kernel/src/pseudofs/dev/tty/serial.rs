use alloc::{collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_driver::serial::{
    self as ax_serial, BIrqHandler, BRxQueue, BTxQueue, InterruptMask, SerialDevice, SerialEvent,
    SerialRuntimePortControl,
};
use ax_errno::AxResult;
use ax_kspin::SpinNoIrq;
use ax_runtime::hal::irq::{AutoEnable, IrqHandle, IrqRequest, ShareMode};
use ax_task::WaitQueue;
use axpoll::PollSet;
use spin::LazyLock;
use starry_process::Process;

use super::{
    Tty,
    ntty::N_TTY,
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
    number: usize,
    control: SpinNoIrq<SerialRuntimePortControl>,
    tx: SpinNoIrq<BTxQueue>,
    rx: SpinNoIrq<BRxQueue>,
    rx_buffer: SpinNoIrq<VecDeque<u8>>,
    irq_handler: Option<BIrqHandler>,
    irq_num: Option<usize>,
    irq_handle: SpinNoIrq<Option<IrqHandle>>,
    irq_armed: AtomicBool,
    input_source: Arc<PollSet>,
    irq_pending: AtomicBool,
    irq_wq: WaitQueue,
    rx_dropped: AtomicUsize,
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
        .unwrap_or_else(|| N_TTY.clone() as Arc<dyn DeviceOps>)
}

pub fn bind_console_to(proc: &Process) -> AxResult<()> {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        return entry.tty.bind_to(proc);
    }
    N_TTY.bind_to(proc)
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

        let selected = selected_console_tty(ax_runtime::hal::dtb::get_chosen_bootargs());

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
            match new_serial_tty(number, serial, selected == Some(number)) {
                Ok(entry) => entries.push(entry),
                Err(err) => warn!("Skipping ttyS{number}: {err:?}"),
            }
        }
        entries.sort_by_key(|entry| entry.number);

        let console_index = selected.and_then(|number| {
            entries
                .iter()
                .position(|entry| entry.number == number)
                .or_else(|| {
                    warn!("bootargs console=ttyS{number} did not match a discovered serial TTY");
                    None
                })
        });
        if let (Some(_), Some(index)) = (selected, console_index) {
            let number = entries[index].number;
            info!("/dev/console bound to ttyS{number}");
        }

        Self {
            entries,
            console_index,
        }
    }
}

fn new_serial_tty(
    number: usize,
    serial: SerialDevice,
    is_boot_console: bool,
) -> AxResult<SerialTtyEntry> {
    let tty_name = format!("ttyS{number}");
    let input_source = Arc::new(PollSet::new());
    let runtime = serial.into_runtime_port()?;
    let name = runtime.name().into();
    let info = runtime.info().clone();
    let irq_num = runtime.irq_num();
    let rx_polling_required = info.rx_polling_required;
    let (control, tx, rx, irq_handler) = runtime.split();
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        number,
        control: SpinNoIrq::new(control),
        tx: SpinNoIrq::new(tx),
        rx: SpinNoIrq::new(rx),
        rx_buffer: SpinNoIrq::new(VecDeque::with_capacity(SERIAL_RX_BUFFER_CAP)),
        irq_handler,
        irq_num,
        irq_handle: SpinNoIrq::new(None),
        irq_armed: AtomicBool::new(false),
        input_source,
        irq_pending: AtomicBool::new(false),
        irq_wq: WaitQueue::new(),
        rx_dropped: AtomicUsize::new(0),
    });
    let process_mode = serial_process_mode(&backend, is_boot_console, rx_polling_required)
        .unwrap_or_else(|| {
            backend.enable_polling_state_sync();
            if is_boot_console {
                ProcessMode::Manual
            } else {
                ProcessMode::Inactive
            }
        });
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

fn serial_process_mode(
    backend: &Arc<SerialBackend>,
    is_boot_console: bool,
    rx_polling_required: bool,
) -> Option<ProcessMode> {
    let irq_num = backend.irq_num?;
    if !is_boot_console {
        return None;
    }
    if rx_polling_required {
        info!(
            "{} requires polling RX despite irq {irq_num}; using polling mode",
            backend.tty_name
        );
        return None;
    }
    if backend.irq_handler.is_none() {
        warn!(
            "{} has irq {irq_num} but no serial IRQ handler; using polling mode",
            backend.tty_name
        );
        return None;
    }

    let data = NonNull::new(Arc::into_raw(backend.clone()) as *mut ()).unwrap();
    let request = IrqRequest::new(serial_raw_irq_handler, data)
        .share_mode(ShareMode::Shared)
        .auto_enable(AutoEnable::No);
    let handle = match ax_runtime::hal::irq::request_irq(irq_num, request) {
        Ok(handle) => handle,
        Err(err) => {
            warn!(
                "Failed to register {} IRQ handler for irq {irq_num}: {err:?}; using polling mode",
                backend.tty_name,
            );
            unsafe {
                Arc::decrement_strong_count(data.as_ptr() as *const SerialBackend);
            }
            return None;
        }
    };
    *backend.irq_handle.lock() = Some(handle);
    spawn_serial_irq_drain(backend.clone());
    Some(ProcessMode::InterruptDriven(backend.input_source.clone()))
}

fn process_mode_name(mode: &ProcessMode) -> &'static str {
    match mode {
        ProcessMode::Manual => "polling",
        ProcessMode::Inactive => "inactive",
        ProcessMode::InterruptDriven(_) => "interrupt",
        ProcessMode::Passive(_) => "passive",
    }
}

fn spawn_serial_irq_drain(backend: Arc<SerialBackend>) {
    let task_name = format!("{}-irq-drain", backend.tty_name);
    ax_task::spawn_with_name(
        move || loop {
            backend
                .irq_wq
                .wait_until(|| backend.irq_pending.swap(false, Ordering::AcqRel));
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
                backend.input_source.wake();
            }
        },
        task_name,
    );
}

impl SerialBackend {
    fn enable_polling_state_sync(&self) {
        if self.irq_handler.is_some() {
            self.control
                .lock()
                .set_irq_mask(InterruptMask::RX_AVAILABLE | InterruptMask::TX_EMPTY);
        }
    }

    fn sync_irq_state(&self) -> SerialEvent {
        let status = self
            .irq_handler
            .as_ref()
            .map(|handler| handler.handle_irq())
            .unwrap_or_else(SerialEvent::empty);
        if status.intersects(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN) {
            self.notify_rx_drain();
        }
        status
    }

    fn notify_rx_drain(&self) {
        self.irq_pending.store(true, Ordering::Release);
        self.irq_wq.notify_one(true);
    }

    fn arm_irq(&self) {
        let Some(irq_num) = self.irq_num else {
            return;
        };
        let Some(handle) = *self.irq_handle.lock() else {
            return;
        };
        if self.irq_armed.swap(true, Ordering::AcqRel) {
            return;
        }

        self.control.lock().enable_rx_interrupts();
        if let Err(err) = ax_runtime::hal::irq::enable_irq(handle) {
            self.control.lock().disable_rx_interrupts();
            self.irq_armed.store(false, Ordering::Release);
            warn!(
                "Failed to arm {} IRQ handler for irq {irq_num}: {err:?}; using polling wakeups",
                self.tty_name
            );
        } else {
            info!("{} IRQ armed on irq {irq_num}", self.tty_name);
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
        self.sync_irq_state();
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

            total += self.push_rx_buffer(&chunk[..read]);
            if read < limit {
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
        if status.intersects(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN) {
            backend.notify_rx_drain();
        }
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

        if !self.backend.irq_armed.load(Ordering::Acquire) {
            return self.backend.read_hardware_rx(buf, true);
        }

        self.backend.notify_rx_drain();
        0
    }
}

impl TtyWrite for SerialWriter {
    fn write(&self, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let mut tx = self.backend.tx.lock();
        let mut written = 0;
        while written < buf.len() {
            self.backend.control.lock().enable_tx_interrupts();
            self.backend.sync_irq_state();
            let next = tx.try_write(&buf[written..]);
            written += next;
            if next == 0 {
                drop(tx);
                ax_task::yield_now();
                tx = self.backend.tx.lock();
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

fn selected_console_tty(bootargs: Option<&str>) -> Option<usize> {
    bootargs?
        .split_ascii_whitespace()
        .filter_map(|arg| arg.strip_prefix("console="))
        .find_map(parse_serial_console)
}

fn parse_serial_console(spec: &str) -> Option<usize> {
    let name = spec.split(',').next().unwrap_or(spec);
    name.strip_prefix("ttyS")?.parse::<usize>().ok()
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

#[cfg(test)]
mod tests {
    use super::{assign_tty_numbers, selected_console_tty};

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
    fn bootargs_select_first_serial_console_even_when_later_console_is_non_serial() {
        assert_eq!(
            selected_console_tty(Some("console=ttyS2,1500000 console=tty1")),
            Some(2)
        );
    }

    #[test]
    fn bootargs_missing_or_non_serial_console_falls_back() {
        assert_eq!(selected_console_tty(None), None);
        assert_eq!(
            selected_console_tty(Some("root=/dev/vda console=tty1")),
            None
        );
    }

    #[test]
    fn malformed_serial_console_does_not_panic() {
        assert_eq!(selected_console_tty(Some("console=ttySx")), None);
    }
}
