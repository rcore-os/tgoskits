use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ax_lazyinit::LazyLock;
use ax_runtime::{
    RuntimeError,
    console::{self, TaskConsoleInput},
    serial::{
        Config, ConfigError, ModemOutputs, RxItem, SerialRuntimeHandle, SerialRxSubscription,
        SerialTaskOutput,
    },
};
use axfs_ng_vfs::{VfsError, VfsResult};

use super::{
    Tty,
    terminal::{
        Terminal,
        ldisc::{ProcessMode, TtyConfig, TtyRead, TtyWrite},
        termios::Termios2,
    },
};
use crate::{StarryError, StarryResult, pseudofs::DeviceOps, sync::Mutex};

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
    terminal: Arc<Terminal>,
    is_console: bool,
    lifecycle_lock: Mutex<()>,
    started: AtomicBool,
    // Keep the physical rate across a software-only B0 transition.
    // Last accepted nominal rate. The UART may round its hardware divisor.
    configured_baudrate: AtomicU32,
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

    fn poll_source(&self) -> Arc<axpoll_set::PollSet> {
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

    fn ioctl(
        &self,
        _current: &crate::task::UserTaskRef,
        _cmd: u32,
        _arg: usize,
    ) -> VfsResult<usize> {
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
    let initial_baudrate = runtime.initial_baudrate();
    let mut termios = initial_serial_termios(initial_baudrate);
    if let Some(framing) = runtime.initial_framing() {
        termios.set_framing(framing);
    }
    let terminal = Arc::new(Terminal::with_termios(termios));
    let backend = Arc::new(SerialBackend {
        name,
        tty_name: tty_name.clone(),
        number,
        runtime,
        output,
        input,
        terminal: terminal.clone(),
        is_console,
        lifecycle_lock: Mutex::new(()),
        started: AtomicBool::new(is_console),
        configured_baudrate: AtomicU32::new(initial_baudrate),
    });

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
        tty_name,
        info.firmware_path,
        info.alias_index,
        info.address.base(),
        info.irq,
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
        let initial_baudrate = self.runtime.info().initial_baudrate;
        let baudrate = startup_baudrate(initial_baudrate);
        let mut termios = *self.terminal.load_termios();
        let result = match self.runtime.start(Config::new().baudrate(baudrate)) {
            Ok(()) => {
                self.configured_baudrate.store(baudrate, Ordering::Relaxed);
                if initial_baudrate == 0 {
                    termios.set_baudrate(baudrate);
                }
                Ok(())
            }
            Err(RuntimeError::SerialConfig(ConfigError::BaudrateUnavailable)) => {
                // Firmware owns the input clock. Keep its divisor when opening the TTY.
                self.runtime.start(Config::new())
            }
            result => result,
        };
        if let Err(err) = result {
            warn!(
                "{} failed to start serial port {}: {:?}",
                self.tty_name, self.name, err
            );
            return Err(err.into());
        }
        if let Some(framing) = self.runtime.initial_framing() {
            termios.set_framing(framing);
        }
        *self.terminal.termios.lock_irqsave() = Arc::new(termios);
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

fn initial_serial_termios(baudrate: u32) -> Termios2 {
    if baudrate == 0 {
        Termios2::with_unknown_baudrate()
    } else {
        Termios2::with_baudrate(baudrate)
    }
}

fn startup_baudrate(current: u32) -> u32 {
    if current == 0 {
        SERIAL_DEFAULT_BAUDRATE
    } else {
        current
    }
}

fn serial_config_update(
    old: &Termios2,
    new: &Termios2,
    configured_baudrate: u32,
) -> Option<Config> {
    // An omitted baudrate preserves the current divisor, including when only
    // firmware knows the input clock.
    let baudrate = new
        .baudrate()
        .filter(|rate| Some(*rate) != old.baudrate() && *rate != configured_baudrate);
    let framing = new.framing();
    let framing_changed = old.framing() != framing;
    let modem_outputs = (old.is_hangup() != new.is_hangup()).then_some(ModemOutputs {
        dtr: !new.is_hangup(),
        rts: !new.is_hangup(),
    });
    (framing_changed || baudrate.is_some() || modem_outputs.is_some()).then_some(Config {
        baudrate,
        data_bits: framing_changed.then_some(framing.data_bits),
        stop_bits: framing_changed.then_some(framing.stop_bits),
        parity: framing_changed.then_some(framing.parity),
        modem_outputs,
    })
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

    fn update_termios(
        &self,
        old: &Termios2,
        new: &Termios2,
        drain: bool,
        before_configure: &mut dyn FnMut() -> StarryResult<()>,
        publish: &mut dyn FnMut(&Termios2),
    ) -> StarryResult<()> {
        self.backend.ensure_started()?;
        let configured_baudrate = self.backend.configured_baudrate.load(Ordering::Relaxed);
        let config = serial_config_update(old, new, configured_baudrate);
        let requested_baudrate = config.as_ref().and_then(|config| config.baudrate);
        self.backend
            .output
            .reconfigure_with(config, drain, before_configure, || {
                if let Some(rate) = requested_baudrate {
                    self.backend
                        .configured_baudrate
                        .store(rate, Ordering::Relaxed);
                }
                let mut termios = *new;
                termios.use_output_baudrate_for_input();
                publish(&termios);
            })
    }
}

#[cfg(all(test, axtest))]
#[axtest::axtest]
fn first_serial_termios_preserves_unchanged_framing() -> axtest::AxTestResult {
    use ax_runtime::serial::{DataBits, Framing, Parity, StopBits};
    use axtest::{ax_assert, ax_assert_eq};

    let configured_baudrate = 115_200;
    let defaults = Termios2::with_baudrate(configured_baudrate);
    let unknown = initial_serial_termios(0);
    ax_assert_eq!(unknown.baudrate(), None);
    ax_assert_eq!(
        serial_config_update(&unknown, &Termios2::default(), 0).and_then(|config| config.baudrate),
        Some(38_400)
    );
    let mut current = defaults;
    let firmware = Framing {
        data_bits: DataBits::Seven,
        stop_bits: StopBits::One,
        parity: Parity::Even,
    };
    current.set_framing(firmware);
    ax_assert_eq!(current.framing(), firmware);
    ax_assert_eq!(current.baudrate(), Some(115_200));
    ax_assert!(serial_config_update(&current, &current, configured_baudrate).is_none());

    let mut hung_up = current;
    hung_up.set_baudrate(0);
    ax_assert_eq!(hung_up.baudrate(), None);
    for (old, new, asserted) in [(&current, &hung_up, false), (&hung_up, &current, true)] {
        let config = serial_config_update(old, new, configured_baudrate);
        ax_assert_eq!(
            config.map(|config| (config.baudrate, config.modem_outputs)),
            Some((
                None,
                Some(ModemOutputs {
                    dtr: asserted,
                    rts: asserted
                })
            ))
        );
    }
    let mut changed_rate = current;
    changed_rate.set_baudrate(57_600);
    ax_assert_eq!(
        serial_config_update(&hung_up, &changed_rate, configured_baudrate)
            .and_then(|config| config.baudrate),
        Some(57_600)
    );

    let config = serial_config_update(&current, &defaults, configured_baudrate);
    ax_assert_eq!(
        config.map(|config| (
            config.baudrate,
            config.data_bits,
            config.stop_bits,
            config.parity
        )),
        Some((
            None,
            Some(DataBits::Eight),
            Some(StopBits::One),
            Some(Parity::None)
        ))
    );
    axtest::AxTestResult::Ok
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn zero_hardware_baudrate_uses_runtime_default() {
        assert_eq!(super::startup_baudrate(0), super::SERIAL_DEFAULT_BAUDRATE);
        assert_eq!(super::startup_baudrate(1_500_000), 1_500_000);
    }
}
