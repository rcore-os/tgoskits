use std::{fs::File, io::Write, path::Path, process::ExitStatus, time::Duration};

use anyhow::Context;

#[derive(Clone, Copy, Default)]
pub(super) struct ChildResourceUsage {
    user_micros: i128,
    system_micros: i128,
    major_faults: i128,
    minor_faults: i128,
    voluntary_context_switches: i128,
    involuntary_context_switches: i128,
}

impl ChildResourceUsage {
    fn delta_since(self, before: Self) -> Self {
        Self {
            user_micros: nonnegative_delta(self.user_micros, before.user_micros),
            system_micros: nonnegative_delta(self.system_micros, before.system_micros),
            major_faults: nonnegative_delta(self.major_faults, before.major_faults),
            minor_faults: nonnegative_delta(self.minor_faults, before.minor_faults),
            voluntary_context_switches: nonnegative_delta(
                self.voluntary_context_switches,
                before.voluntary_context_switches,
            ),
            involuntary_context_switches: nonnegative_delta(
                self.involuntary_context_switches,
                before.involuntary_context_switches,
            ),
        }
    }

    fn user_seconds(self) -> f64 {
        self.user_micros as f64 / 1_000_000.0
    }

    fn system_seconds(self) -> f64 {
        self.system_micros as f64 / 1_000_000.0
    }
}

pub(super) fn write_host_time_metrics(
    path: &Path,
    elapsed: Duration,
    usage_start: Option<ChildResourceUsage>,
    usage_end: Option<ChildResourceUsage>,
    status: &ExitStatus,
) -> anyhow::Result<()> {
    let mut file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let elapsed_seconds = elapsed.as_secs_f64();
    writeln!(file, "Elapsed time: {elapsed_seconds:.6}")?;
    if let (Some(start), Some(end)) = (usage_start, usage_end) {
        let usage = end.delta_since(start);
        let user_seconds = usage.user_seconds();
        let system_seconds = usage.system_seconds();
        writeln!(file, "User time: {user_seconds:.6}")?;
        writeln!(file, "System time: {system_seconds:.6}")?;
        if elapsed_seconds > 0.0 {
            let cpu_percent = (user_seconds + system_seconds) / elapsed_seconds * 100.0;
            writeln!(file, "Percent of CPU this job got: {cpu_percent:.2}%")?;
        }
        writeln!(file, "Major page faults: {}", usage.major_faults)?;
        writeln!(file, "Minor page faults: {}", usage.minor_faults)?;
        writeln!(
            file,
            "Voluntary context switches: {}",
            usage.voluntary_context_switches
        )?;
        writeln!(
            file,
            "Involuntary context switches: {}",
            usage.involuntary_context_switches
        )?;
    } else {
        writeln!(file, "User time: unavailable")?;
        writeln!(file, "System time: unavailable")?;
    }
    writeln!(file, "Exit status: {}", exit_status_code(status))?;
    Ok(())
}

pub(super) fn write_host_perf_unavailable(path: &Path, reason: &str) -> anyhow::Result<()> {
    let mut file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    writeln!(file, "# host perf unavailable: {reason}")?;
    writeln!(
        file,
        "# host perf stat measures the host QEMU process; it is not a guest PMU counter"
    )?;
    Ok(())
}

pub(super) fn exit_status_code(status: &ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| if status.success() { 0 } else { 1 })
}

fn nonnegative_delta(after: i128, before: i128) -> i128 {
    after.saturating_sub(before).max(0)
}

#[cfg(unix)]
pub(super) fn child_resource_usage() -> Option<ChildResourceUsage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the provided rusage pointer when it returns 0.
    if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: getrusage returned success, so usage is initialized.
    let usage = unsafe { usage.assume_init() };
    Some(ChildResourceUsage {
        user_micros: timeval_micros(usage.ru_utime),
        system_micros: timeval_micros(usage.ru_stime),
        major_faults: usage.ru_majflt.into(),
        minor_faults: usage.ru_minflt.into(),
        voluntary_context_switches: usage.ru_nvcsw.into(),
        involuntary_context_switches: usage.ru_nivcsw.into(),
    })
}

#[cfg(unix)]
fn timeval_micros(value: libc::timeval) -> i128 {
    i128::from(value.tv_sec) * 1_000_000 + i128::from(value.tv_usec)
}

#[cfg(not(unix))]
pub(super) fn child_resource_usage() -> Option<ChildResourceUsage> {
    None
}
