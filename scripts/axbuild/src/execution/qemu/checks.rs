//! Execute caller-owned checks while retaining one QEMU and serial lifetime.
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ostool::run::qemu::QemuConfig;

use crate::execution::serial_checks::{SerialChecks, SerialMonitor};

/// A member deadline also bounds readiness, sends and result observation.
pub(crate) fn member_deadline(start: Instant, budget: Duration, group: Instant) -> Result<Instant> {
    Ok(start
        .checked_add(budget)
        .context("member deadline overflow")?
        .min(group))
}

pub(crate) struct QemuChecks {
    monitor: Option<SerialMonitor>,
    worker: Option<JoinHandle<Result<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    _directory: tempfile::TempDir,
}

impl QemuChecks {
    pub(crate) fn start(
        qemu: &mut QemuConfig,
        run: impl FnOnce(&mut SerialChecks, Instant, Arc<AtomicBool>) -> Result<Vec<u8>>
        + Send
        + 'static,
    ) -> Result<Self> {
        let budget = Duration::from_secs(
            qemu.timeout
                .filter(|s| *s > 0)
                .context("ordered checks require a finite group timeout")?,
        );
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("serial.sock");
        let qmp: PathBuf = directory.path().join("qmp.sock");
        let argument = format!("unix:{},server=on,wait=on", socket.display());
        if let Some(i) = qemu.args.iter().position(|s| s == "-serial") {
            *qemu
                .args
                .get_mut(i + 1)
                .context("-serial requires an argument")? = argument;
        } else {
            qemu.args.extend(["-serial".into(), argument]);
        }
        qemu.args.extend([
            "-qmp".into(),
            format!("unix:{},server=on,wait=off", qmp.display()),
        ]);
        let stop = Arc::new(AtomicBool::new(false));
        let (monitor, mut serial) = SerialMonitor::start(
            &socket,
            &qemu.fail_regex,
            stop.clone(),
            budget,
            std::io::stdout(),
        )?;
        qemu.shell_check_steps.clear();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            let result = (|| {
                let start = serial.connected_at()?;
                let deadline = start
                    .checked_add(budget)
                    .context("group deadline overflow")?;
                run(&mut serial, deadline, worker_stop)
            })();
            if let Err(error) = super::control::request_qmp_quit(&qmp) {
                eprintln!("QMP shutdown: {error:#}");
            }
            result
        });
        Ok(Self {
            monitor: Some(monitor),
            worker: Some(worker),
            stop,
            _directory: directory,
        })
    }

    pub(crate) fn finish(mut self) -> Result<Vec<u8>> {
        self.stop.store(true, Ordering::Release);
        let result = self
            .worker
            .take()
            .context("missing check worker")?
            .join()
            .map_err(|_| anyhow::anyhow!("check worker panicked"))?;
        self.monitor
            .take()
            .context("missing serial monitor")?
            .finish()?;
        result
    }
}

impl Drop for QemuChecks {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Optional serial-only member boundaries, declared by the owning runtime config.
pub(crate) fn prepare_serial_groups(
    qemu: &mut QemuConfig,
    path: &std::path::Path,
) -> Result<Option<QemuChecks>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Group {
        name: String,
        step_count: usize,
        timeout: u64,
    }
    #[derive(serde::Deserialize)]
    struct Groups {
        #[serde(default)]
        shell_check_groups: Vec<Group>,
    }
    let groups: Groups = toml::from_str(&std::fs::read_to_string(path)?)?;
    if groups.shell_check_groups.is_empty() {
        return Ok(None);
    }
    let mut steps = qemu.shell_check_steps.iter();
    let mut members = Vec::new();
    for group in groups.shell_check_groups {
        anyhow::ensure!(
            group.step_count > 0 && group.timeout > 0 && !group.name.trim().is_empty(),
            "invalid serial check group in {}",
            path.display()
        );
        let member: Vec<_> = steps.by_ref().take(group.step_count).cloned().collect();
        anyhow::ensure!(
            member.len() == group.step_count,
            "serial group {} exceeds declared steps",
            group.name
        );
        members.push((group.name, Duration::from_secs(group.timeout), member));
    }
    anyhow::ensure!(
        steps.next().is_none(),
        "serial groups must cover every step in {}",
        path.display()
    );
    Ok(Some(QemuChecks::start(
        qemu,
        move |serial, group_deadline, _stop| {
            for (index, (name, budget, steps)) in members.into_iter().enumerate() {
                let start = if index == 0 {
                    serial.connected_at()?
                } else {
                    Instant::now()
                };
                serial
                    .run_until(&steps, member_deadline(start, budget, group_deadline)?)
                    .with_context(|| format!("serial member {name}"))?;
            }
            Ok(Vec::new())
        },
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn member_budget_is_shared_by_all_phases_and_capped_by_the_group() {
        let start = Instant::now();
        let group = start + Duration::from_secs(60);
        let deadline = member_deadline(start, Duration::from_secs(10), group).unwrap();
        assert_eq!(deadline, start + Duration::from_secs(10));
        // Seven seconds consumed by readiness leave three for the probe.
        assert_eq!(
            deadline.duration_since(start + Duration::from_secs(7)),
            Duration::from_secs(3)
        );
        assert_eq!(
            member_deadline(start, Duration::from_secs(90), group).unwrap(),
            group
        );
    }
}
