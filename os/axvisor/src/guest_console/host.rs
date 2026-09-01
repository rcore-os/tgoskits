//! Physical host-console ownership.

use alloc::format;
use core::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use ax_std::os::arceos::modules::ax_runtime::console::{
    self, ConsoleLogDropReport, ConsoleLogRecord, ConsoleLogSubscription, TaskConsoleInput,
    TaskConsoleOutput,
};
use ax_std::os::arceos::{
    modules::{
        ax_runtime::{RuntimeError, RuntimeResult, emergency_console},
        ax_task::IrqNotify,
    },
    sync::NoPreemptMutex,
};
use std::sync::{Mutex, OnceLock};

use axvisor::console_mux::HostOutputQueue;

use super::terminal::TerminalNewlineNormalizer;

const HOST_OUTPUT_QUEUE_CAPACITY: usize = 64 * 1024;
const HOST_OUTPUT_BATCH_CAPACITY: usize = 512;

struct HostConsole {
    input: Option<TaskConsoleInput>,
    logs: Option<ConsoleLogSubscription>,
    #[cfg(feature = "test-console-atomic-output")]
    test_output: TaskConsoleOutput,
}

struct HostOutput {
    queue: NoPreemptMutex<HostOutputQueue<HOST_OUTPUT_QUEUE_CAPACITY>>,
    ready: IrqNotify,
    failed: AtomicBool,
}

struct HostOutputBatch {
    bytes: [u8; HOST_OUTPUT_BATCH_CAPACITY],
    len: usize,
    dropped_bytes: usize,
}

static HOST_CONSOLE: OnceLock<HostConsole> = OnceLock::new();
static HOST_CONSOLE_CONFIGURE: Mutex<()> = Mutex::new(());
static HOST_OUTPUT: HostOutput = HostOutput::new();

/// Takes the sole task-context input and log subscription before any vCPU starts.
///
/// The returned runtime output capability is moved into one dedicated task.
/// Every guest/vCPU producer only submits to [`HOST_OUTPUT`] and therefore can
/// never acquire a sleepable lock or wait for UART backpressure.
pub(crate) fn configure_host_console() -> Result<()> {
    let _configure_guard = HOST_CONSOLE_CONFIGURE
        .lock()
        .map_err(|_| anyhow!("host console configuration lock was poisoned"))?;
    if HOST_CONSOLE.get().is_some() {
        bail!("host console has already been configured");
    }

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
    let output = console::output().context("failed to open host console output")?;
    let host_console = HostConsole {
        input,
        logs,
        #[cfg(feature = "test-console-atomic-output")]
        test_output: output.clone(),
    };
    std::thread::Builder::new()
        .name("axvisor-console-output".into())
        .spawn(move || run_host_output_worker(output))
        .context("failed to start host console output worker")?;
    let Ok(()) = HOST_CONSOLE.set(host_console) else {
        unreachable!("the configuration lock serializes the only HOST_CONSOLE publisher");
    };
    Ok(())
}

fn host_console() -> Option<&'static HostConsole> {
    HOST_CONSOLE.get()
}

/// Reads at most one byte from the physical host console.
///
/// No other Axvisor component may own the runtime RX subscription.
pub(crate) fn read_host_byte() -> Option<u8> {
    let input = host_console()?.input.as_ref()?;
    let mut item = [console::RxItem::default()];
    while input.try_read(&mut item) != 0 {
        if let console::RxItem::Byte { byte, .. } = item[0] {
            return Some(byte);
        }
    }
    None
}

pub(crate) fn read_host_log() -> Option<ConsoleLogRecord> {
    host_log_subscription()?.try_read()
}

pub(crate) fn take_host_log_drops() -> ConsoleLogDropReport {
    host_log_subscription().map_or_else(
        ConsoleLogDropReport::default,
        ConsoleLogSubscription::dropped,
    )
}

fn host_log_subscription() -> Option<&'static ConsoleLogSubscription> {
    host_console()?.logs.as_ref()
}

/// Sleeps until physical input or a host log record is published.
///
pub(crate) fn wait_for_host_event() {
    let Some(console) = host_console() else {
        park_console_task();
    };
    let logs = host_log_subscription();
    let result = match (&console.input, logs) {
        (Some(input), Some(logs)) => input.wait_event(logs),
        (Some(input), None) => input.wait_readable(),
        (None, Some(logs)) => logs.wait_readable(),
        (None, None) => park_console_task(),
    };
    if let Err(err) = result {
        log::error!("runtime console stopped while Axvisor was running: {err}");
        park_console_task();
    }
}

fn park_console_task() -> ! {
    static STOPPED: ax_std::os::arceos::modules::ax_task::WaitQueue =
        ax_std::os::arceos::modules::ax_task::WaitQueue::new();
    loop {
        STOPPED.wait();
    }
}

/// Submits bytes without allocating, sleeping, or touching UART registers.
pub(crate) fn submit_host_bytes(bytes: &[u8]) {
    if host_console().is_none() {
        return;
    }
    HOST_OUTPUT.submit(bytes);
}

/// Builds one non-interleaved host-output transaction in the fixed queue.
///
/// `transaction` runs while the queue's non-sleeping lock is held. It must not
/// allocate, sleep, or call back into a physical console API.
pub(crate) fn submit_host_transaction(transaction: impl FnOnce(&mut dyn FnMut(&[u8]))) {
    if host_console().is_none() {
        return;
    }
    HOST_OUTPUT.submit_transaction(transaction);
}

fn run_host_output_worker(output: TaskConsoleOutput) {
    let mut terminal = TerminalNewlineNormalizer::new();
    loop {
        HOST_OUTPUT.ready.wait();
        if HOST_OUTPUT.failed.load(Ordering::Acquire) {
            return;
        }
        loop {
            let batch = HOST_OUTPUT.take_batch();
            if batch.is_empty() {
                break;
            }
            if let Err(error) = write_host_output_batch(&output, &mut terminal, &batch) {
                HOST_OUTPUT.failed.store(true, Ordering::Release);
                let _ = emergency_console::write_fmt(format_args!(
                    "\r\nAxvisor host console output stopped: {error}\r\n"
                ));
                return;
            }
        }
    }
}

fn write_host_output_batch(
    output: &TaskConsoleOutput,
    terminal: &mut TerminalNewlineNormalizer,
    batch: &HostOutputBatch,
) -> RuntimeResult {
    if batch.dropped_bytes != 0 {
        let report = format!(
            "\n[Axvisor host console dropped {} queued bytes]\n",
            batch.dropped_bytes
        );
        terminal.write(report.as_bytes(), |bytes| {
            output.write_all(bytes).map(|_| ())
        })?;
    }
    terminal.write(&batch.bytes[..batch.len], |bytes| {
        output.write_all(bytes).map(|_| ())
    })
}

impl HostOutput {
    const fn new() -> Self {
        Self {
            queue: NoPreemptMutex::new(HostOutputQueue::new()),
            ready: IrqNotify::new(),
            failed: AtomicBool::new(false),
        }
    }

    fn submit(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.submit_transaction(|emit| emit(bytes));
    }

    fn submit_transaction(&self, transaction: impl FnOnce(&mut dyn FnMut(&[u8]))) {
        if self.failed.load(Ordering::Acquire) {
            return;
        }

        // No hard-IRQ path uses this queue. Disabling preemption is sufficient
        // for vCPU callbacks and avoids extending local IRQ-off latency across
        // a complete producer transaction.
        let mut queue = self.queue.lock();
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        let mut transaction_queue = queue.begin_transaction();
        transaction(&mut |bytes| {
            transaction_queue.enqueue(bytes);
        });
        let submitted = transaction_queue.has_activity();
        drop(transaction_queue);
        drop(queue);
        if submitted {
            self.ready.notify_irq();
        }
    }

    fn take_batch(&self) -> HostOutputBatch {
        let mut batch = HostOutputBatch {
            bytes: [0; HOST_OUTPUT_BATCH_CAPACITY],
            len: 0,
            dropped_bytes: 0,
        };
        let mut queue = self.queue.lock();
        batch.dropped_bytes = queue.take_dropped_bytes();
        batch.len = queue.dequeue(&mut batch.bytes);
        batch
    }
}

impl HostOutputBatch {
    fn is_empty(&self) -> bool {
        self.len == 0 && self.dropped_bytes == 0
    }
}

#[cfg(feature = "test-console-atomic-output")]
/// Fills the bounded runtime ingress while the single owner CPU is held under
/// `PreemptGuard`.
pub(crate) fn fill_runtime_output_queue() {
    static FRAME: [u8; 256] = [b'x'; 256];
    const MAX_EXPECTED_RUNTIME_INGRESS: usize = 64 * 1024;

    assert_eq!(
        ax_std::os::arceos::modules::ax_hal::cpu_num(),
        1,
        "atomic-output regression requires the runtime owner and test on one CPU"
    );
    let mut accepted = 0;
    while accepted <= MAX_EXPECTED_RUNTIME_INGRESS {
        match host_console()
            .expect("host console must be configured before running the regression")
            .test_output
            .try_write(&FRAME)
        {
            Ok(written) => accepted += written,
            Err(RuntimeError::WouldBlock) => return,
            Err(error) => panic!("failed to fill runtime output queue: {error}"),
        }
    }
    panic!("runtime output accepted more than its bounded ingress contract");
}
