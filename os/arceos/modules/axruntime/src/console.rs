//! Sleepable task-context access to the runtime-owned physical console.
//!
//! This layer owns console selection and handoff. It deliberately exposes raw
//! receive items and complete log records; line discipline and terminal ABI
//! policy remain in the consuming OS.

use alloc::{boxed::Box, sync::Arc};
use core::{
    fmt::{self, Write},
    task::Waker,
};

use ax_lazyinit::OnceLock;
use ax_sync::Mutex;
use axpoll_set::PollSet;

pub use crate::serial::RxItem;
use crate::{
    RuntimeError, RuntimeResult,
    raw_console::RawConsoleInput,
    serial,
    structured_log::{RuntimeLogContext, write_record},
    task::sync::RawSpinLock,
};

static ACTIVATION: OnceLock<ConsoleActivation> = OnceLock::new();
static TTY_NUMBERS: OnceLock<Box<[Option<usize>]>> = OnceLock::new();
// Task writers take these in order. Log publishers take only the hardware
// lock, so early CPUs and interrupt context never touch task-owned state.
static RAW_OUTPUT_LOCK: Mutex<()> = Mutex::new(());
static RAW_HARDWARE_LOCK: RawSpinLock<()> = RawSpinLock::new(());
static RAW_OUTPUT_SOURCE: OnceLock<Arc<PollSet>> = OnceLock::new();

/// Result of selecting the firmware console before secondary CPUs start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsoleActivation {
    /// One serial runtime owns the physical console.
    Active {
        runtime_index: usize,
        tty_number: usize,
    },
    /// No compatible runtime UART was discovered; the HAL console remains the
    /// sole owner.
    RawHal(ConsoleUnavailable),
    /// Raw access was disabled for an explicit virtual selection or to release
    /// an unidentified firmware UART before ordinary ports become available.
    Disabled,
    /// No runtime may use the former early console after this point.
    FailedClosed(ConsoleUnavailable),
}

/// Why no task-context serial console was activated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsoleUnavailable {
    NoSerialDevice,
    NoHardwareSelected,
    SelectedDeviceNotFound,
    UnspecifiedHardwareConsole,
    NoNumberedSerial,
    HandoffFailed,
    RuntimeStartFailed,
    LogRoutingBusy,
}

/// Selects, starts, and commits the sole runtime console before SMP startup.
pub(crate) fn activate_before_smp() -> ConsoleActivation {
    if let Some(activation) = ACTIVATION.get().copied() {
        return activation;
    }

    let runtimes = serial::runtimes();
    let tty_numbers = initialize_tty_numbers(runtimes);
    let selected = ax_hal::console::device_id_with_serial_index(|number| {
        tty_numbers
            .iter()
            .position(|assigned| *assigned == Some(number))
            .map(|index| runtimes[index].info().device_id)
    });
    let selection = select_runtime(runtimes, tty_numbers, selected);
    let (runtime_index, tty_number) = match selection {
        Ok(selection) => selection,
        Err(unavailable) => return activate_fallback(unavailable),
    };

    let runtime = &runtimes[runtime_index];
    if runtime.begin_console_handoff().is_err() {
        return fail_closed(runtime, ConsoleUnavailable::HandoffFailed);
    }
    if runtime.start_handoff_console().is_err() {
        return fail_closed(runtime, ConsoleUnavailable::RuntimeStartFailed);
    }
    if let Err(error) = runtime.commit_console_handoff() {
        let reason = if error == RuntimeError::SerialConsoleBusy {
            ConsoleUnavailable::LogRoutingBusy
        } else {
            ConsoleUnavailable::HandoffFailed
        };
        return fail_closed(runtime, reason);
    }

    let activation = ConsoleActivation::Active {
        runtime_index,
        tty_number,
    };
    ACTIVATION.call_once(|| activation);
    activation
}

fn activate_fallback(reason: ConsoleUnavailable) -> ConsoleActivation {
    let unknown_mmio = serial::runtimes().iter().any(|runtime| {
        matches!(
            runtime.info().address,
            ax_driver::serial::SerialRegisterAddress::Mmio(_)
        )
    }) && ax_hal::console::early_console_has_unknown_mmio();
    let activation = unavailable_activation(reason, unknown_mmio, || {
        // No task has acquired a raw-HAL input or output capability yet. Stop
        // opaque firmware access before exposing ordinary ports; otherwise no
        // MMIO UART could safely start. Keep a known early UART as the fallback.
        let handoff = ax_hal::console::begin_runtime_handoff()
            .and_then(|()| ax_hal::console::commit_runtime_handoff());
        if handoff.is_err() {
            ax_hal::console::fail_runtime_handoff_closed();
        }
        handoff
    });
    ACTIVATION.call_once(|| activation);
    activation
}

fn unavailable_activation(
    reason: ConsoleUnavailable,
    unknown_mmio: bool,
    release_early_console: impl FnOnce() -> ax_hal::console::ConsoleHandoffResult,
) -> ConsoleActivation {
    if reason != ConsoleUnavailable::NoHardwareSelected && !unknown_mmio {
        return ConsoleActivation::RawHal(reason);
    }
    match release_early_console() {
        Ok(()) => ConsoleActivation::Disabled,
        Err(_) => ConsoleActivation::FailedClosed(ConsoleUnavailable::HandoffFailed),
    }
}

fn fail_closed(
    runtime: &serial::SerialRuntimeHandle,
    reason: ConsoleUnavailable,
) -> ConsoleActivation {
    runtime.fail_console_closed();
    ax_hal::console::fail_runtime_handoff_closed();
    let activation = ConsoleActivation::FailedClosed(reason);
    ACTIVATION.call_once(|| activation);
    activation
}

fn select_runtime(
    runtimes: &[serial::SerialRuntimeHandle],
    tty_numbers: &[Option<usize>],
    selected: ax_hal::console::ConsoleDeviceIdResult,
) -> Result<(usize, usize), ConsoleUnavailable> {
    let candidates = runtimes
        .iter()
        .zip(tty_numbers.iter().copied())
        .map(|(runtime, tty_number)| (runtime.info().device_id, tty_number))
        .collect::<alloc::vec::Vec<_>>();
    select_candidate(
        &candidates,
        selected,
        ax_hal::console::can_handoff_unspecified_console(),
    )
}

fn select_candidate(
    candidates: &[(ax_hal::console::ConsoleDeviceId, Option<usize>)],
    selected: ax_hal::console::ConsoleDeviceIdResult,
    allow_unspecified_handoff: bool,
) -> Result<(usize, usize), ConsoleUnavailable> {
    if candidates.is_empty() {
        return Err(ConsoleUnavailable::NoSerialDevice);
    }
    match selected {
        Ok(device_id) => candidates
            .iter()
            .position(|(candidate, _)| *candidate == device_id)
            .and_then(|index| Some((index, candidates[index].1?)))
            .ok_or(ConsoleUnavailable::SelectedDeviceNotFound),
        Err(ax_hal::console::ConsoleDeviceIdError::NotSpecified) if !allow_unspecified_handoff => {
            Err(ConsoleUnavailable::UnspecifiedHardwareConsole)
        }
        Err(ax_hal::console::ConsoleDeviceIdError::NotSpecified) => candidates
            .iter()
            .enumerate()
            .filter_map(|(index, (_, number))| Some((index, (*number)?)))
            .min_by_key(|&(_, number)| number)
            .ok_or(ConsoleUnavailable::NoNumberedSerial),
        Err(ax_hal::console::ConsoleDeviceIdError::NoHardwareDevice) => {
            Err(ConsoleUnavailable::NoHardwareSelected)
        }
        Err(ax_hal::console::ConsoleDeviceIdError::DeviceNotFound) => {
            Err(ConsoleUnavailable::SelectedDeviceNotFound)
        }
    }
}

/// Returns the Linux-compatible `ttyS` number assigned to a serial runtime.
pub fn tty_number(runtime: &serial::SerialRuntimeHandle) -> Option<usize> {
    let index = serial::runtimes()
        .iter()
        .position(|candidate| candidate.info().device_id == runtime.info().device_id)?;
    TTY_NUMBERS.get()?.get(index).copied().flatten()
}

fn initialize_tty_numbers(runtimes: &[serial::SerialRuntimeHandle]) -> &'static [Option<usize>] {
    TTY_NUMBERS.call_once(|| {
        assign_tty_numbers(
            &runtimes
                .iter()
                .map(|runtime| runtime.info().alias_index)
                .collect::<alloc::vec::Vec<_>>(),
        )
        .into_boxed_slice()
    })
}

fn activation() -> Option<ConsoleActivation> {
    ACTIVATION.get().copied()
}

/// Returns whether this runtime owns the active physical console.
pub fn is_active(runtime: &serial::SerialRuntimeHandle) -> bool {
    serial::active_console()
        .is_some_and(|active| active.info().device_id == runtime.info().device_id)
}

fn inactive_console_error(activation: Option<ConsoleActivation>) -> RuntimeError {
    match activation {
        Some(ConsoleActivation::FailedClosed(_)) | Some(ConsoleActivation::Active { .. }) => {
            RuntimeError::ConsoleFailedClosed
        }
        Some(ConsoleActivation::Disabled) => RuntimeError::OperationNotSupported,
        Some(ConsoleActivation::RawHal(_)) | None => RuntimeError::SerialNotStarted,
    }
}

/// Takes the unique raw input capability for the active console.
pub fn take_input() -> RuntimeResult<TaskConsoleInput> {
    if let Some(runtime) = serial::active_console() {
        return runtime
            .take_rx_subscription()
            .map(|inner| TaskConsoleInput {
                inner: TaskConsoleInputInner::Runtime(inner),
            })
            .ok_or(RuntimeError::SerialConsoleBusy);
    }
    match activation() {
        Some(ConsoleActivation::RawHal(_)) => Ok(TaskConsoleInput {
            inner: TaskConsoleInputInner::RawHal(crate::raw_console::take_input()?),
        }),
        activation => Err(inactive_console_error(activation)),
    }
}

/// Returns a cloneable output capability for the active console.
pub fn output() -> RuntimeResult<TaskConsoleOutput> {
    if let Some(runtime) = serial::active_console() {
        return Ok(TaskConsoleOutput {
            inner: TaskConsoleOutputInner::Runtime(runtime.task_output()),
        });
    }
    match activation() {
        Some(ConsoleActivation::RawHal(_)) => Ok(TaskConsoleOutput {
            inner: TaskConsoleOutputInner::RawHal,
        }),
        activation => Err(inactive_console_error(activation)),
    }
}

/// Takes the unique complete-log-record subscription for the active console.
pub fn subscribe_logs() -> RuntimeResult<ConsoleLogSubscription> {
    let runtime = serial::active_console().ok_or_else(|| match activation() {
        Some(ConsoleActivation::RawHal(_)) => RuntimeError::OperationNotSupported,
        activation => inactive_console_error(activation),
    })?;
    runtime
        .take_log_subscription()
        .map(|inner| ConsoleLogSubscription { inner })
        .ok_or(RuntimeError::SerialConsoleBusy)
}

/// Serializes one ordinary log record with raw-HAL task output when no runtime
/// UART exists. Failed-closed ownership consumes the record without touching
/// the former early console.
pub(crate) fn try_publish_without_runtime(
    meta: ax_log::RecordMeta,
    context: RuntimeLogContext,
    args: fmt::Arguments<'_>,
) -> Option<ax_log::PublishStatus> {
    match activation()? {
        ConsoleActivation::RawHal(_) => {
            // Logging can run on a secondary CPU before its scheduler has
            // installed a current task, or from interrupt context. A
            // sleepable mutex is therefore never a valid record arbiter.
            Some(publish_raw_record(meta, context, args, &mut RawHalWriter))
        }
        ConsoleActivation::Active { .. }
        | ConsoleActivation::Disabled
        | ConsoleActivation::FailedClosed(_) => Some(ax_log::PublishStatus::Dropped),
    }
}

fn publish_raw_record(
    meta: ax_log::RecordMeta,
    context: RuntimeLogContext,
    args: fmt::Arguments<'_>,
    writer: &mut impl Write,
) -> ax_log::PublishStatus {
    let Some(_hardware) = RAW_HARDWARE_LOCK.try_lock_irqsave() else {
        return ax_log::PublishStatus::Dropped;
    };
    if write_record(writer, meta, context, args).is_ok() {
        ax_log::PublishStatus::Published
    } else {
        ax_log::PublishStatus::Dropped
    }
}

/// Unique raw RX capability. It performs no CR/LF or terminal transformation.
pub struct TaskConsoleInput {
    inner: TaskConsoleInputInner,
}

enum TaskConsoleInputInner {
    Runtime(serial::SerialRxSubscription),
    RawHal(RawConsoleInput),
}

impl TaskConsoleInput {
    pub fn try_read(&self, out: &mut [RxItem]) -> usize {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => inner.drain(out),
            TaskConsoleInputInner::RawHal(inner) => inner.try_read(out),
        }
    }

    pub fn wait_readable(&self) -> RuntimeResult {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => inner.wait_readable(),
            TaskConsoleInputInner::RawHal(inner) => {
                inner.wait_readable();
                Ok(())
            }
        }
    }

    pub fn read(&self, out: &mut [RxItem]) -> RuntimeResult<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let read = self.try_read(out);
            if read != 0 {
                return Ok(read);
            }
            self.wait_readable()?;
        }
    }

    pub fn discard_pending(&self) -> RuntimeResult {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => inner.discard_pending(),
            TaskConsoleInputInner::RawHal(inner) => {
                inner.discard_pending();
                Ok(())
            }
        }
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => inner.poll_source(),
            TaskConsoleInputInner::RawHal(inner) => inner.poll_source(),
        }
    }

    /// Sleeps until either RX or a complete subscribed log record is ready.
    pub fn wait_event(&self, logs: &ConsoleLogSubscription) -> RuntimeResult {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => inner.wait_console_event(&logs.inner),
            TaskConsoleInputInner::RawHal(inner) => {
                inner.wait_readable();
                Ok(())
            }
        }
    }

    /// Returns a waker for task-context publishers of external readiness.
    ///
    /// Publish the condition before waking, and include that condition in
    /// [`Self::wait_event_until`]. Waking alone does not make the wait complete.
    /// The waker keeps its console runtime alive, without retaining this RX lease.
    pub fn event_waker(&self) -> Waker {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => inner.event_waker(),
            TaskConsoleInputInner::RawHal(inner) => inner.event_waker(),
        }
    }

    /// Waits without a timer for RX, optional logs, or external readiness.
    ///
    /// `ready` runs in task context without the wait-queue lock. It must observe
    /// durable state published before calling [`Self::event_waker`]'s waker.
    /// The queue's registration protocol prevents a lost wake between checking
    /// that state and parking. The predicate must not consume the readiness.
    /// Runtime shutdown remains an error; raw-HAL input rejects log subscriptions.
    pub fn wait_event_until(
        &self,
        logs: Option<&ConsoleLogSubscription>,
        ready: impl Fn() -> bool,
    ) -> RuntimeResult {
        match &self.inner {
            TaskConsoleInputInner::Runtime(inner) => {
                inner.wait_console_event_until(logs.map(|logs| &logs.inner), ready)
            }
            TaskConsoleInputInner::RawHal(inner) => {
                if logs.is_some() {
                    return Err(RuntimeError::OperationNotSupported);
                }
                inner.wait_readable_until(ready);
                Ok(())
            }
        }
    }
}

fn raw_output_source() -> Arc<PollSet> {
    RAW_OUTPUT_SOURCE
        .call_once(|| Arc::new(PollSet::new()))
        .clone()
}

/// Cloneable output capability. All clones share one sleepable output lock.
#[derive(Clone)]
pub struct TaskConsoleOutput {
    inner: TaskConsoleOutputInner,
}

#[derive(Clone)]
enum TaskConsoleOutputInner {
    Runtime(serial::SerialTaskOutput),
    RawHal,
}

impl TaskConsoleOutput {
    pub fn try_write(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.try_write(bytes),
            TaskConsoleOutputInner::RawHal => {
                let Some(_output) = RAW_OUTPUT_LOCK.try_lock() else {
                    return Err(RuntimeError::WouldBlock);
                };
                let Some(_hardware) = RAW_HARDWARE_LOCK.try_lock_irqsave() else {
                    return Err(RuntimeError::WouldBlock);
                };
                ax_hal::console::write_bytes(bytes);
                Ok(bytes.len())
            }
        }
    }

    pub fn write_all(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.write_all(bytes),
            TaskConsoleOutputInner::RawHal => {
                let _output = RAW_OUTPUT_LOCK.lock();
                let _hardware = RAW_HARDWARE_LOCK.lock_irqsave();
                ax_hal::console::write_bytes(bytes);
                Ok(bytes.len())
            }
        }
    }

    pub fn write_text_all(&self, bytes: &[u8]) -> RuntimeResult<usize> {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.write_text_all(bytes),
            TaskConsoleOutputInner::RawHal => {
                let _output = RAW_OUTPUT_LOCK.lock();
                let _hardware = RAW_HARDWARE_LOCK.lock_irqsave();
                ax_hal::console::write_text_bytes(bytes);
                Ok(bytes.len())
            }
        }
    }

    pub fn write_fmt(&self, args: fmt::Arguments<'_>) -> fmt::Result {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.write_fmt(args),
            TaskConsoleOutputInner::RawHal => {
                let _output = RAW_OUTPUT_LOCK.lock();
                let _hardware = RAW_HARDWARE_LOCK.lock_irqsave();
                RawHalWriter.write_fmt(args)
            }
        }
    }

    pub fn drain(&self) -> RuntimeResult {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.wait_idle(),
            TaskConsoleOutputInner::RawHal => {
                let _output = RAW_OUTPUT_LOCK.lock();
                let _hardware = RAW_HARDWARE_LOCK.lock_irqsave();
                Ok(())
            }
        }
    }

    pub fn discard_pending(&self) -> RuntimeResult {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.discard_pending(),
            TaskConsoleOutputInner::RawHal => {
                let _output = RAW_OUTPUT_LOCK.lock();
                let _hardware = RAW_HARDWARE_LOCK.lock_irqsave();
                Ok(())
            }
        }
    }

    /// Serializes an optional drain/configuration transaction with all writers.
    pub fn reconfigure(
        &self,
        config: Option<serial::Config>,
        drain: bool,
        publish: impl FnOnce(),
    ) -> RuntimeResult {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.reconfigure(config, drain, publish),
            TaskConsoleOutputInner::RawHal => {
                let _output = RAW_OUTPUT_LOCK.lock();
                let _hardware = RAW_HARDWARE_LOCK.lock_irqsave();
                if config.is_some() {
                    return Err(RuntimeError::OperationNotSupported);
                }
                let _ = drain;
                publish();
                Ok(())
            }
        }
    }

    pub fn poll_source(&self) -> Arc<PollSet> {
        match &self.inner {
            TaskConsoleOutputInner::Runtime(inner) => inner.poll_source(),
            TaskConsoleOutputInner::RawHal => raw_output_source(),
        }
    }
}

struct RawHalWriter;

impl Write for RawHalWriter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        ax_hal::console::write_text_bytes(text.as_bytes());
        Ok(())
    }
}

/// One complete record produced by the shared kernel logger.
pub struct ConsoleLogRecord {
    inner: serial::LogRecord,
}

impl ConsoleLogRecord {
    /// Application routing tag for raw output; absent for kernel logs/prints.
    pub fn output_tag(&self) -> Option<u128> {
        match self.inner.kind() {
            serial::LogRecordKind::Output(tag) => Some(tag),
            _ => None,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        self.inner.bytes()
    }

    pub fn cpu_id(&self) -> usize {
        self.inner.cpu_id()
    }

    pub fn timestamp_nanos(&self) -> u64 {
        self.inner.timestamp_nanos()
    }

    pub fn task_id(&self) -> Option<u64> {
        self.inner.task_id()
    }

    pub fn is_truncated(&self) -> bool {
        self.inner.is_truncated()
    }

    pub fn is_log(&self) -> bool {
        self.inner.kind() == serial::LogRecordKind::Log
    }
}

/// Records discarded because the subscriber did not keep up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConsoleLogDropReport {
    pub records: usize,
    pub source_bytes: usize,
}

/// Unique optional complete-record log subscription.
pub struct ConsoleLogSubscription {
    inner: serial::SerialLogSubscription,
}

impl ConsoleLogSubscription {
    /// Returns the task-context waker paired with [`Self::wait_readable_until`].
    /// Publish external readiness before waking; the waker retains the runtime,
    /// but does not keep this log subscription active.
    pub fn event_waker(&self) -> Waker {
        self.inner.event_waker()
    }

    /// Waits without a timer for a record, drop report, or external readiness.
    ///
    /// `ready` follows the non-consuming publish-before-wake contract of
    /// [`TaskConsoleInput::wait_event_until`]. Shutdown remains an error.
    pub fn wait_readable_until(&self, ready: impl Fn() -> bool) -> RuntimeResult {
        self.inner.wait_readable_until(ready)
    }

    /// Enqueues raw bytes alongside kernel records in publication order.
    ///
    /// `tag` is opaque to the runtime and identifies the application stream and
    /// its generation. Adjacent writes with the same tag may coalesce. This
    /// operation allocates nothing and never sleeps. A full queue rejects the
    /// entire write with `WouldBlock`; the producer retains ownership and must
    /// retry instead of reporting the bytes as dropped host logs.
    pub fn write_output(&self, tag: u128, bytes: &[u8]) -> RuntimeResult {
        self.inner.write_output(tag, bytes)
    }

    pub fn try_read(&self) -> Option<ConsoleLogRecord> {
        self.inner
            .try_read()
            .map(|inner| ConsoleLogRecord { inner })
    }

    pub fn dropped(&self) -> ConsoleLogDropReport {
        let (records, source_bytes) = self.inner.dropped();
        ConsoleLogDropReport {
            records,
            source_bytes,
        }
    }

    pub fn wait_readable(&self) -> RuntimeResult {
        self.inner.wait_readable()
    }
}

fn assign_tty_numbers(alias_indices: &[Option<usize>]) -> alloc::vec::Vec<Option<usize>> {
    let mut assigned = alloc::vec![None; alias_indices.len()];
    let mut used = alloc::vec::Vec::new();

    for (device_index, alias) in alias_indices.iter().copied().enumerate() {
        let Some(number) = alias else {
            continue;
        };
        if used.contains(&number) {
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
    use ax_hal::console::ConsoleDeviceIdError;

    use super::{
        ACTIVATION, ConsoleActivation, ConsoleUnavailable, assign_tty_numbers,
        inactive_console_error, output, select_candidate, take_input, unavailable_activation,
    };
    use crate::RuntimeError;

    #[test]
    fn tty_numbering_preserves_aliases_and_fills_gaps() {
        assert_eq!(
            assign_tty_numbers(&[Some(0), None, Some(2), None]),
            [Some(0), Some(1), Some(2), Some(3)]
        );
        assert_eq!(
            assign_tty_numbers(&[Some(1), Some(1), None]),
            [Some(1), Some(0), Some(2)]
        );
    }

    #[test]
    fn firmware_device_id_wins_over_ttys0() {
        let tty_s0 = rdrive::DeviceId::from(10);
        let tty_s1 = rdrive::DeviceId::from(11);
        assert_eq!(
            select_candidate(&[(tty_s0, Some(0)), (tty_s1, Some(1))], Ok(tty_s1), false),
            Ok((1, 1))
        );
    }

    #[test]
    fn only_not_specified_allows_runtime_fallback() {
        let tty_s0 = rdrive::DeviceId::from(10);
        let candidates = [(tty_s0, Some(0))];
        assert_eq!(
            select_candidate(&candidates, Err(ConsoleDeviceIdError::NotSpecified), true),
            Ok((0, 0))
        );
        assert_eq!(
            select_candidate(
                &candidates,
                Err(ConsoleDeviceIdError::NoHardwareDevice),
                true
            ),
            Err(ConsoleUnavailable::NoHardwareSelected)
        );
        assert_eq!(
            select_candidate(&candidates, Err(ConsoleDeviceIdError::DeviceNotFound), true),
            Err(ConsoleUnavailable::SelectedDeviceNotFound)
        );
    }

    #[test]
    fn unspecified_console_can_use_a_nonzero_serial_number() {
        let tty_s3 = rdrive::DeviceId::from(13);
        let tty_s2 = rdrive::DeviceId::from(12);
        assert_eq!(
            select_candidate(
                &[(tty_s3, Some(3)), (tty_s2, Some(2))],
                Err(ConsoleDeviceIdError::NotSpecified),
                true
            ),
            Ok((1, 2))
        );
        for selected in [
            ConsoleDeviceIdError::NotSpecified,
            ConsoleDeviceIdError::NoHardwareDevice,
        ] {
            assert_eq!(
                select_candidate(&[], Err(selected), true),
                Err(ConsoleUnavailable::NoSerialDevice)
            );
        }
    }

    #[test]
    fn unspecified_console_without_platform_support_keeps_the_raw_hal_owner() {
        let device = rdrive::DeviceId::from(12);
        for number in [0, 2] {
            let reason = select_candidate(
                &[(device, Some(number))],
                Err(ConsoleDeviceIdError::NotSpecified),
                ax_hal::console::can_handoff_unspecified_console(),
            )
            .expect_err("a serial number does not identify the early console");
            assert_eq!(reason, ConsoleUnavailable::UnspecifiedHardwareConsole);
            assert_eq!(
                unavailable_activation(reason, false, || panic!("must not begin console handoff")),
                ConsoleActivation::RawHal(reason)
            );
        }
    }

    #[test]
    fn unavailable_runtime_selection_keeps_the_raw_hal_owner() {
        for reason in [
            ConsoleUnavailable::NoSerialDevice,
            ConsoleUnavailable::SelectedDeviceNotFound,
            ConsoleUnavailable::NoNumberedSerial,
        ] {
            assert_eq!(
                unavailable_activation(reason, false, || panic!("fallback must retain raw access")),
                ConsoleActivation::RawHal(reason)
            );
        }
    }

    #[test]
    fn fallback_releases_raw_access_before_exposing_ports_with_unknown_ownership() {
        for (reason, unknown_mmio) in [
            (ConsoleUnavailable::NoHardwareSelected, false),
            (ConsoleUnavailable::SelectedDeviceNotFound, true),
            (ConsoleUnavailable::NoNumberedSerial, true),
        ] {
            let released = core::cell::Cell::new(false);
            let activation = unavailable_activation(reason, unknown_mmio, || {
                released.set(true);
                Ok(())
            });
            assert!(released.get());
            assert_eq!(activation, ConsoleActivation::Disabled);
            assert_eq!(
                unavailable_activation(reason, unknown_mmio, || {
                    Err(ax_hal::console::ConsoleHandoffError::InvalidState)
                }),
                ConsoleActivation::FailedClosed(ConsoleUnavailable::HandoffFailed)
            );
        }
    }

    #[test]
    fn failed_closed_console_never_falls_back_to_the_raw_hal() {
        assert_eq!(
            inactive_console_error(Some(ConsoleActivation::FailedClosed(
                ConsoleUnavailable::HandoffFailed,
            ))),
            RuntimeError::ConsoleFailedClosed
        );
        assert_eq!(
            inactive_console_error(Some(ConsoleActivation::RawHal(
                ConsoleUnavailable::NoSerialDevice,
            ))),
            RuntimeError::SerialNotStarted
        );
    }

    #[test]
    fn raw_hal_without_irq_does_not_fake_sleepable_input() {
        ACTIVATION.call_once(|| ConsoleActivation::RawHal(ConsoleUnavailable::NoSerialDevice));
        assert!(matches!(
            take_input(),
            Err(RuntimeError::OperationNotSupported)
        ));
        assert!(output().is_ok());
    }
}
