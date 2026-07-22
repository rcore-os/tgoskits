use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_errno::{AxError, AxResult};
use ax_runtime::{
    hal::console::{ConsoleDeviceIdError, ConsoleDeviceIdResult},
    serial::{
        Config, DataBits, Parity, RxFlag, RxItem, SerialRuntimeHandle, SerialRxSubscription,
        SerialTxSender, StopBits,
    },
};
use ax_sync::Mutex;
use rdrive::DeviceId as RDriveDeviceId;
use spin::LazyLock;
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
    runtime: SerialRuntimeHandle,
    tx: SerialTxSender,
    rx: SerialRxSubscription,
    lifecycle_lock: Mutex<()>,
    started: AtomicBool,
    output_lock: Mutex<()>,
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
        entry.backend.runtime.activate_console_output()?;
        ax_runtime::hal::console::claim_runtime_output();
        return entry.tty.bind_to(proc);
    }
    Err(AxError::NoSuchDevice)
}

pub fn arm_console_irq() {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        let _ = entry.backend.ensure_started();
    }
}

impl SerialRegistry {
    fn discover() -> Self {
        let serials = ax_runtime::serial::runtimes();
        let numbers = assign_tty_numbers(
            serials
                .iter()
                .map(|serial| serial.info().alias_index)
                .collect::<Vec<_>>()
                .as_slice(),
        );

        let mut entries = Vec::new();
        for (serial, number) in serials.iter().cloned().zip(numbers) {
            let Some(number) = number else {
                warn!(
                    "Skipping serial device {} at {} because ttyS number could not be assigned",
                    serial.info().name,
                    serial.info().firmware_path
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

fn new_serial_tty(number: usize, runtime: SerialRuntimeHandle) -> AxResult<SerialTtyEntry> {
    let tty_name = format!("ttyS{number}");
    let info = runtime.info().clone();
    let name = info.name.clone();
    let rdrive_device_id = info.device_id;
    let tx = runtime.tx_sender();
    let rx = runtime.take_rx_subscription().ok_or(AxError::BadState)?;
    let input_source = rx.poll_source();
    let output_source = tx.poll_source();
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        rdrive_device_id,
        number,
        runtime,
        tx,
        rx,
        lifecycle_lock: Mutex::new(()),
        started: AtomicBool::new(false),
        output_lock: Mutex::new(()),
    });

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
                input: input_source,
                output: Some(output_source),
            },
        },
    );
    info!(
        "{} registered: path={}, alias={:?}, paddr={:#x}, irq={:?}",
        tty_name, info.firmware_path, info.alias_index, info.paddr, info.irq,
    );
    Ok(SerialTtyEntry {
        number,
        tty,
        backend: entry_backend,
    })
}

impl SerialBackend {
    fn ensure_started(&self) -> AxResult<()> {
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        let _lifecycle = self.lifecycle_lock.lock();
        if self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = self
            .runtime
            .start(Config::new().baudrate(startup_baudrate(self.runtime.info().initial_baudrate)));
        if let Err(err) = result {
            warn!(
                "{} failed to start serial port {}: {:?}",
                self.tty_name, self.name, err
            );
            return Err(err);
        }
        self.started.store(true, Ordering::Release);
        Ok(())
    }

    fn drain_tx(&self) -> AxResult<()> {
        self.ensure_started()?;
        let _guard = self.output_lock.lock();
        self.tx.wait_idle()
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.rx.drain(out)
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
            match self.backend.tx.try_write(&buf[written..]) {
                Ok(count) => written += count,
                Err(AxError::WouldBlock) => {
                    if self.backend.tx.wait_writable().is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
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
        self.backend.tx.try_write(buf).unwrap_or(0)
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
            .runtime
            .set_config(serial_config_from_termios(new))
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
