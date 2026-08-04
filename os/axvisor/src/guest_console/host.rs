//! Physical host-console ownership and transport selection.

use alloc::boxed::Box;

use anyhow::{Result, bail};
use ax_runtime::{
    hal::console::ConsoleDeviceIdError,
    serial::{Config, RxItem, SerialRuntimeHandle, SerialRxSubscription, SerialTxSender},
};
use spin::Once;

const DEFAULT_SERIAL_BAUDRATE: u32 = 115_200;

static HOST_CONSOLE: Once<Box<dyn HostConsoleTransport>> = Once::new();

trait HostConsoleTransport: Send + Sync {
    fn write_bytes(&self, bytes: &[u8]);
    fn read_byte(&self) -> Option<u8>;
    fn wait_for_input(&self);
}

struct RuntimeHostConsole {
    rx: SerialRxSubscription,
    tx: SerialTxSender,
}

struct PlatformPollingHostConsole;

/// Transfers a hardware console to the generic serial runtime when possible.
///
/// Firmware-less consoles such as SBI remain on the platform polling path. A
/// firmware-selected hardware UART must have one exact runtime owner; an absent
/// match is an initialization error rather than a guessed fallback.
pub(crate) fn configure_host_console() -> Result<()> {
    HOST_CONSOLE.try_call_once(select_host_console).map(|_| ())
}

/// Reads at most one byte from the physical host console.
///
/// No other Axvisor component may call the platform console input API.
pub(crate) fn read_host_byte() -> Option<u8> {
    HOST_CONSOLE.get()?.read_byte()
}

/// Waits for input without making a continuously runnable polling task.
pub(crate) fn wait_for_host_input() {
    if let Some(console) = HOST_CONSOLE.get() {
        console.wait_for_input();
    } else {
        std::thread::yield_now();
    }
}

pub(super) fn write_host_bytes(bytes: &[u8]) {
    if let Some(console) = HOST_CONSOLE.get() {
        console.write_bytes(bytes);
    } else {
        ax_hal::console::write_bytes(bytes);
    }
}

fn select_host_console() -> Result<Box<dyn HostConsoleTransport>> {
    match ax_hal::console::device_id() {
        Ok(device_id) => {
            let runtime = ax_runtime::serial::runtimes()
                .iter()
                .find(|runtime| runtime.info().device_id == device_id)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "firmware-selected host console {device_id:?} has no serial runtime"
                    )
                })?;
            RuntimeHostConsole::claim(runtime)
                .map(|console| Box::new(console) as Box<dyn HostConsoleTransport>)
        }
        Err(ConsoleDeviceIdError::NotSpecified | ConsoleDeviceIdError::NoHardwareDevice) => {
            info!("host console uses the platform polling transport");
            Ok(Box::new(PlatformPollingHostConsole))
        }
        Err(ConsoleDeviceIdError::DeviceNotFound) => {
            bail!("firmware-selected host console device was not probed")
        }
    }
}

impl RuntimeHostConsole {
    fn claim(runtime: SerialRuntimeHandle) -> Result<Self> {
        let rx = runtime.take_rx_subscription().ok_or_else(|| {
            anyhow::anyhow!(
                "host console runtime {} already has an RX owner",
                runtime.info().name
            )
        })?;
        let baudrate = match runtime.info().initial_baudrate {
            0 => DEFAULT_SERIAL_BAUDRATE,
            baudrate => baudrate,
        };
        runtime
            .start(Config::new().baudrate(baudrate))
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to start host console runtime {}: {error:?}",
                    runtime.info().name
                )
            })?;
        if let Err(error) = runtime.claim_console_output() {
            let rollback = runtime.shutdown();
            bail!(
                "failed to claim host console runtime {}: {error:?}; shutdown rollback: \
                 {rollback:?}",
                runtime.info().name
            );
        }

        let tx = runtime.tx_sender();
        info!(
            "host console runtime owns {} at {}",
            runtime.info().name,
            runtime.info().firmware_path
        );
        Ok(Self { rx, tx })
    }
}

impl HostConsoleTransport for RuntimeHostConsole {
    fn write_bytes(&self, bytes: &[u8]) {
        let _ = self.tx.write_all(bytes);
    }

    fn read_byte(&self) -> Option<u8> {
        loop {
            let mut item = [RxItem::default()];
            if self.rx.drain(&mut item) == 0 {
                return None;
            }
            if let RxItem::Byte { byte, .. } = item[0] {
                return Some(byte);
            }
        }
    }

    fn wait_for_input(&self) {
        let _ = self.rx.wait_readable();
    }
}

impl HostConsoleTransport for PlatformPollingHostConsole {
    fn write_bytes(&self, bytes: &[u8]) {
        ax_hal::console::write_bytes(bytes);
    }

    fn read_byte(&self) -> Option<u8> {
        let mut byte = [0u8; 1];
        (ax_hal::console::read_bytes(&mut byte) == 1).then_some(byte[0])
    }

    fn wait_for_input(&self) {
        // Static SBI consoles have no serial IRQ runtime. Yielding is the
        // narrow fallback until the platform exposes a blocking input event.
        std::thread::yield_now();
    }
}
