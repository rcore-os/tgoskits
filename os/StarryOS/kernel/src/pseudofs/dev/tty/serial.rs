use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_lazyinit::LazyLock;
use ax_runtime::{
    console::{self, TaskConsoleInput},
    serial::{
        Config, DataBits, Parity, RxItem, SerialRuntimeHandle, SerialRxSubscription,
        SerialTaskOutput, StopBits,
    },
};
use axfs_ng_vfs::{VfsError, VfsResult};

use super::{
    Tty,
    terminal::{
        Terminal,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
        termios::{Termios2, TermiosParity},
    },
};
use crate::{StarryError, StarryResult, pseudofs::DeviceOps, sync::Mutex, task::Process};

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
    number: usize,
    runtime: SerialRuntimeHandle,
    output: SerialTaskOutput,
    input: SerialInput,
    is_console: bool,
    lifecycle_lock: Mutex<()>,
    started: AtomicBool,
}

enum SerialInput {
    Console(TaskConsoleInput),
    Port(SerialRxSubscription),
}

impl SerialInput {
    fn drain(&self, out: &mut [RxItem]) -> usize {
        match self {
            Self::Console(input) => input.try_read(out),
            Self::Port(input) => input.drain(out),
        }
    }

    fn discard_pending(&self) -> ax_runtime::RuntimeResult {
        match self {
            Self::Console(input) => input.discard_pending(),
            Self::Port(input) => input.discard_pending(),
        }
    }

    fn poll_source(&self) -> Arc<axpoll::PollSet> {
        match self {
            Self::Console(input) => input.poll_source(),
            Self::Port(input) => input.poll_source(),
        }
    }
}

struct NoConsole;

impl DeviceOps for NoConsole {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::NoSuchDevice)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::NoSuchDevice)
    }

    fn ioctl(&self, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        Err(VfsError::NoSuchDevice)
    }

    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        Err(VfsError::NoSuchDevice)
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
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

pub fn bind_console_to(proc: &Process) -> StarryResult<()> {
    if let Some(index) = SERIAL_REGISTRY.console_index
        && let Some(entry) = SERIAL_REGISTRY.entries.get(index)
    {
        entry.tty.bind_to(proc)?;
        entry.backend.ensure_started()?;
        return Ok(());
    }
    Err(StarryError::NoSuchDevice)
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

        let mut entries = Vec::new();
        for serial in serials.iter().cloned() {
            let Some(number) = console::tty_number(&serial) else {
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

        let console_index = entries.iter().position(|entry| entry.backend.is_console);
        if let Some(index) = console_index {
            let number = entries[index].number;
            info!("/dev/console bound to runtime console ttyS{number}");
        } else {
            warn!("/dev/console has no serial TTY binding");
        }

        Self {
            entries,
            console_index,
        }
    }
}

fn new_serial_tty(number: usize, runtime: SerialRuntimeHandle) -> StarryResult<SerialTtyEntry> {
    let tty_name = format!("ttyS{number}");
    let info = runtime.info().clone();
    let name = info.name.clone();
    let is_console = console::is_active(&runtime);
    let input = if is_console {
        SerialInput::Console(console::take_input()?)
    } else {
        SerialInput::Port(
            runtime
                .take_rx_subscription()
                .ok_or(StarryError::BadState)?,
        )
    };
    let output = runtime.task_output();
    let input_source = input.poll_source();
    let output_source = output.poll_source();
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        number,
        runtime,
        output,
        input,
        is_console,
        lifecycle_lock: Mutex::new(()),
        started: AtomicBool::new(is_console),
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
    fn ensure_started(&self) -> StarryResult<()> {
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
            return Err(err.into());
        }
        self.started.store(true, Ordering::Release);
        Ok(())
    }

    fn drain_tx(&self) -> StarryResult<()> {
        self.ensure_started()?;
        Ok(self.output.wait_idle()?)
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.input.drain(out)
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

fn termios_requires_reconfigure(old: &Termios2, new: &Termios2) -> bool {
    old.baudrate() != new.baudrate()
        || old.data_bits() != new.data_bits()
        || old.stop_bits() != new.stop_bits()
        || old.parity() != new.parity()
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
                    RxItem::Byte { byte, .. } => {
                        buf[total] = byte;
                        total += 1;
                    }
                    RxItem::Overrun => {}
                }
            }
        }

        total
    }

    fn discard_input(&mut self) -> StarryResult<()> {
        Ok(self.backend.input.discard_pending()?)
    }
}

impl TtyWrite for SerialWriter {
    fn open(&self) -> StarryResult<()> {
        self.backend.ensure_started()
    }

    fn write(&self, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        if self.backend.ensure_started().is_err() {
            return;
        }
        let _ = self.backend.output.write_all(buf);
    }

    fn try_write(&self, buf: &[u8]) -> usize {
        if buf.is_empty() {
            return 0;
        }
        if self.backend.ensure_started().is_err() {
            return 0;
        }
        self.backend.output.try_write(buf).unwrap_or(0)
    }

    fn flush_echo_before_input(&self) -> bool {
        true
    }

    fn max_sync_echo_bytes(&self) -> usize {
        SERIAL_SYNC_ECHO_LIMIT
    }

    fn drain(&self) -> StarryResult<()> {
        self.backend.drain_tx()
    }

    fn discard_output(&self) -> StarryResult<()> {
        self.backend.ensure_started()?;
        Ok(self.backend.output.discard_pending()?)
    }

    fn termios_changed(&self, old: &Termios2, new: &Termios2) -> StarryResult<()> {
        if !termios_requires_reconfigure(old, new) {
            return Ok(());
        }
        self.backend.ensure_started()?;
        Ok(self
            .backend
            .output
            .reconfigure(Some(serial_config_from_termios(new)), false, || {})?)
    }

    fn update_termios(
        &self,
        old: &Termios2,
        new: &Termios2,
        drain: bool,
        publish: &mut dyn FnMut(),
    ) -> StarryResult<()> {
        self.backend.ensure_started()?;
        let config =
            termios_requires_reconfigure(old, new).then(|| serial_config_from_termios(new));
        Ok(self.backend.output.reconfigure(config, drain, publish)?)
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn zero_hardware_baudrate_uses_runtime_default() {
        assert_eq!(super::startup_baudrate(0), super::SERIAL_DEFAULT_BAUDRATE);
        assert_eq!(super::startup_baudrate(1_500_000), 1_500_000);
    }
}
