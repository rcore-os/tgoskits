//! Physical host-console ownership.

use anyhow::{Context, Result, bail};
use ax_std::os::arceos::modules::ax_runtime::RuntimeError;
use ax_std::os::arceos::modules::ax_runtime::console::{
    self, ConsoleLogDropReport, ConsoleLogRecord, ConsoleLogSubscription, TaskConsoleInput,
    TaskConsoleOutput,
};
use std::sync::OnceLock;

struct HostConsole {
    input: Option<TaskConsoleInput>,
    output: TaskConsoleOutput,
    logs: Option<ConsoleLogSubscription>,
}

static HOST_CONSOLE: OnceLock<HostConsole> = OnceLock::new();

/// Takes the sole task-context input and log subscription before any vCPU starts.
pub(crate) fn configure_host_console_reader() -> Result<()> {
    let logs = match console::subscribe_logs() {
        Ok(logs) => Some(logs),
        Err(RuntimeError::OperationNotSupported) => None,
        Err(error) => return Err(error).context("failed to subscribe to host console logs"),
    };
    let input = match console::take_input() {
        Ok(input) => Some(input),
        Err(RuntimeError::OperationNotSupported) => None,
        Err(error) => return Err(error).context("failed to take host console input"),
    };
    let console = HostConsole {
        input,
        output: console::output().context("failed to open host console output")?,
        logs,
    };
    if HOST_CONSOLE.set(console).is_err() {
        bail!("host console has already been configured");
    }
    Ok(())
}

fn host_console() -> &'static HostConsole {
    HOST_CONSOLE
        .get()
        .expect("host console must be configured before VM startup")
}

/// Reads at most one byte from the physical host console.
///
/// No other Axvisor component may own the runtime RX subscription.
pub(crate) fn read_host_byte() -> Option<u8> {
    let input = host_console().input.as_ref()?;
    let mut item = [console::RxItem::default()];
    while input.try_read(&mut item) != 0 {
        if let console::RxItem::Byte { byte, .. } = item[0] {
            return Some(byte);
        }
    }
    None
}

pub(crate) fn read_host_log() -> Option<ConsoleLogRecord> {
    host_console().logs.as_ref()?.try_read()
}

pub(crate) fn take_host_log_drops() -> ConsoleLogDropReport {
    host_console().logs.as_ref().map_or_else(
        ConsoleLogDropReport::default,
        ConsoleLogSubscription::dropped,
    )
}

/// Sleeps until physical input or a host log record is published.
///
/// Returns `false` if the runtime console has stopped and the shell can no
/// longer make progress.
pub(crate) fn wait_for_host_input() -> bool {
    let console = host_console();
    let Some(input) = &console.input else {
        return false;
    };
    let result = match &console.logs {
        Some(logs) => input.wait_event(logs),
        None => input.wait_readable(),
    };
    match result {
        Ok(()) => true,
        Err(err) => {
            log::error!("runtime console stopped while Axvisor was running: {err}");
            false
        }
    }
}

pub(crate) fn write_host_bytes(bytes: &[u8]) {
    if let Err(err) = host_console().output.write_all(bytes) {
        log::error!("failed to write host console bytes: {err}");
    }
}
