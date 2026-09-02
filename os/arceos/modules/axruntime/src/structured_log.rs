use core::{fmt, time::Duration};

const STRUCTURED_LOG_PREFIX_COLOR: &str = "\u{1b}[37m";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContextUnavailable;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeLogContext {
    timestamp: Duration,
    cpu_id: Option<usize>,
    task_id: Option<u64>,
}

impl RuntimeLogContext {
    pub(crate) const fn new(
        timestamp: Duration,
        cpu_id: Option<usize>,
        task_id: Option<u64>,
    ) -> Self {
        Self {
            timestamp,
            cpu_id,
            task_id,
        }
    }
}

pub(crate) fn with_runtime_log_context<R>(
    consume: impl FnOnce(RuntimeLogContext) -> R,
) -> Result<R, ContextUnavailable> {
    let _availability_guard = ax_task::sync::IrqSaveGuard::new();
    // SAFETY: local IRQ exclusion prevents scheduling and migration while the
    // fallible pin is constructed and used. The full guard is entered only
    // after the pin validates CPU-local availability.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            #[cfg(test)]
            record_full_guard_entry();
            let _guard = ax_task::sync::PreemptIrqSaveGuard::new();
            let cpu_id = ax_hal::percpu::this_cpu_id_pinned(pin);
            let current = ax_task::current_may_uninit();
            let task_id = current.as_ref().map(|task| task.id().as_u64());
            let timestamp = ax_hal::time::monotonic_time();
            consume(RuntimeLogContext::new(timestamp, Some(cpu_id), task_id))
        })
    }
    .map_err(|_| ContextUnavailable)
}

#[cfg(test)]
std::thread_local! {
    static FULL_GUARD_ENTRIES: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_full_guard_entry() {
    FULL_GUARD_ENTRIES.set(FULL_GUARD_ENTRIES.get() + 1);
}

#[cfg(test)]
fn take_full_guard_entries() -> usize {
    FULL_GUARD_ENTRIES.replace(0)
}

pub(crate) fn fallback_runtime_log_context(meta: ax_log::RecordMeta) -> RuntimeLogContext {
    let timestamp = match meta.kind() {
        ax_log::RecordKind::Print => Duration::ZERO,
        ax_log::RecordKind::Log => ax_hal::time::monotonic_time(),
    };
    RuntimeLogContext::new(timestamp, None, None)
}

pub(crate) fn write_structured_prefix(
    output: &mut (impl fmt::Write + ?Sized),
    context: RuntimeLogContext,
) -> fmt::Result {
    let seconds = context.timestamp.as_secs();
    let micros = context.timestamp.subsec_micros();
    match (context.cpu_id, context.task_id) {
        (Some(cpu_id), Some(task_id)) => write!(
            output,
            "{STRUCTURED_LOG_PREFIX_COLOR}[{seconds:>3}.{micros:06} {cpu_id}:{task_id} "
        ),
        (Some(cpu_id), None) => write!(
            output,
            "{STRUCTURED_LOG_PREFIX_COLOR}[{seconds:>3}.{micros:06} {cpu_id} "
        ),
        (None, _) => write!(
            output,
            "{STRUCTURED_LOG_PREFIX_COLOR}[{seconds:>3}.{micros:06} "
        ),
    }
}

pub(crate) fn write_record(
    output: &mut (impl fmt::Write + ?Sized),
    meta: ax_log::RecordMeta,
    context: RuntimeLogContext,
    args: fmt::Arguments<'_>,
) -> fmt::Result {
    if meta.kind() == ax_log::RecordKind::Log {
        write_structured_prefix(output, context)?;
    }
    output.write_fmt(args)
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use core::time::Duration;

    use super::{
        RuntimeLogContext, fallback_runtime_log_context, take_full_guard_entries,
        with_runtime_log_context, write_record, write_structured_prefix,
    };

    #[test]
    fn structured_prefix_keeps_available_runtime_metadata() {
        let cases = [
            (
                RuntimeLogContext::new(Duration::new(12, 345_678_000), Some(2), Some(7)),
                "\u{1b}[37m[ 12.345678 2:7 ",
            ),
            (
                RuntimeLogContext::new(Duration::new(12, 345_678_000), Some(2), None),
                "\u{1b}[37m[ 12.345678 2 ",
            ),
            (
                RuntimeLogContext::new(Duration::new(12, 345_678_000), None, None),
                "\u{1b}[37m[ 12.345678 ",
            ),
        ];

        for (context, expected) in cases {
            let mut output = String::new();
            write_structured_prefix(&mut output, context).unwrap();
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn unavailable_context_falls_back_without_entering_the_full_guard() {
        let (rendered, guard_entries) = std::thread::spawn(|| {
            let _ = take_full_guard_entries();
            let meta = ax_log::RecordMeta::log();
            let rendered = with_runtime_log_context(|_| String::new()).unwrap_or_else(|_| {
                let mut output = String::new();
                write_record(
                    &mut output,
                    meta,
                    fallback_runtime_log_context(meta),
                    format_args!("early body\n"),
                )
                .unwrap();
                output
            });
            (rendered, take_full_guard_entries())
        })
        .join()
        .expect("unavailable context capture must not panic");

        assert!(rendered.starts_with("\u{1b}[37m["));
        assert!(rendered.ends_with("early body\n"));
        assert_eq!(guard_entries, 0);
    }
}
