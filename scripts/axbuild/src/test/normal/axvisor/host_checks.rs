//! Host checks share one QEMU lifetime; each keeps its own input directory and deadline.
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use anyhow::Context;
use ostool::run::qemu::QemuConfig;
use serde::Deserialize;

use super::{AxvisorQemuCase, host_probe, types::AxvisorHttpProbeConfig};

pub(super) struct HostChecks {
    guard: Option<host_probe::HostHttpProbeGuard>,
    capture: Option<crate::support::qemu_output_capture::QemuOutputCaptureGuard>,
    _qmp_directory: Option<tempfile::TempDir>,
}

impl HostChecks {
    pub(super) fn finish(self) -> Option<host_probe::HostHttpProbeOutcome> {
        // Stop and reap host children before closing the serial tee: a live
        // child may still hold inherited descriptors for that capture.
        let outcome = self.guard.and_then(host_probe::HostHttpProbeGuard::finish);
        drop(self.capture);
        outcome
    }
    pub(super) fn is_configured(&self) -> bool {
        self.guard.is_some()
    }
}

pub(super) fn prepare(qemu: &mut QemuConfig, case: &AxvisorQemuCase) -> anyhow::Result<HostChecks> {
    let check_plan = crate::execution::case::load_selected_runtime_checks(
        &case.case.qemu_config_path,
        case.selected_checks.as_deref(),
    )?;
    let mut probing = false;
    for check in &check_plan.checks {
        anyhow::ensure!(
            !probing || !check.document.contains_key("shell_check_steps"),
            "serial checks after host probes require an explicit session adapter"
        );
        probing |= check.document.contains_key("host_http_probe");
    }
    if !check_plan.checks.is_empty() {
        println!(
            "[axbuild] boot group={} checks={} serial_steps={}",
            case.case.name,
            check_plan.checks.len(),
            qemu.shell_check_steps.len()
        );
        for check in &check_plan.checks {
            println!(
                "[axbuild] ordered check: {}",
                check
                    .document
                    .get("id")
                    .and_then(toml::Value::as_str)
                    .or_else(|| check.path.file_stem().and_then(|name| name.to_str()))
                    .unwrap_or("unnamed")
            );
        }
    }
    let mut probes = Vec::new();
    if check_plan.checks.is_empty() {
        if let Some(config) = load_axvisor_http_probe_config(&case.case.qemu_config_path)? {
            probes.push((config, case.case.case_dir.clone(), qemu.timeout));
        }
    } else {
        for check in &check_plan.checks {
            if let Some(config) = load_axvisor_http_probe_config(&check.path)? {
                let timeout = check
                    .document
                    .get("timeout")
                    .and_then(toml::Value::as_integer)
                    .map(u64::try_from)
                    .transpose()?;
                probes.push((
                    config,
                    check
                        .path
                        .parent()
                        .context("check has no parent")?
                        .to_path_buf(),
                    timeout,
                ));
            }
        }
    }
    let mut host_probe_guard = None;
    let mut observation_capture = None;
    let mut observation = None;
    let mut qmp_directory = None;
    if let Some((probe_config, ..)) = probes.first() {
        // Passive boot observations can precede a host probe without letting the
        // terminal's first success close QEMU before the probe has executed.
        if !qemu.shell_check_steps.is_empty() {
            anyhow::ensure!(
                qemu.shell_check_steps.len() == 1,
                "host probe batches currently accept one leading passive observation"
            );
            let step = &qemu.shell_check_steps[0];
            anyhow::ensure!(
                step.shell_cmd.is_none() && step.shell_prefix.is_none(),
                "cannot interleave interactive serial commands and host probes"
            );
            let patterns = step
                .success_regex
                .as_ref()
                .context("passive observation requires a success condition")?;
            for pattern in patterns {
                regex::Regex::new(pattern)?;
            }
            if let Some(failures) = &step.fail_regex {
                qemu.fail_regex.extend(failures.iter().cloned());
            }
            let output = crate::support::qemu_success::QemuSuccessOutput::new(patterns);
            observation_capture = Some(
                crate::support::qemu_output_capture::QemuOutputCaptureGuard::install(Some(
                    output.clone(),
                ))?,
            );
            let timeout = check_plan
                .checks
                .first()
                .and_then(|check| check.document.get("timeout"))
                .and_then(toml::Value::as_integer)
                .map(u64::try_from)
                .transpose()?
                .context("passive probe prerequisite requires its original timeout")?;
            observation = Some((
                output,
                std::time::Instant::now()
                    .checked_add(std::time::Duration::from_secs(timeout))
                    .context("passive observation timeout exceeds the host clock range")?,
            ));
            qemu.shell_check_steps.clear();
        }
        let mut probe_config = probe_config.clone();
        if let Some((_, deadline)) = &observation {
            probe_config.connect_timeout_secs = probe_config.connect_timeout_secs.min(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_secs()
                    .max(1),
            );
        }
        let host_port = pick_free_local_port()?;
        let directory = tempfile::tempdir()?;
        let qmp_socket = directory.path().join("qmp.sock");
        qmp_directory = Some(directory);
        // Each QEMU option and its value must be a separate argv element
        // (QEMU takes the value of `-netdev`/`-device` from the following
        // argument), matching how the `.toml` config stores them.
        qemu.args.extend([
            "-netdev".to_string(),
            format!(
                "user,id=net0,hostfwd=tcp:127.0.0.1:{host_port}-:{}",
                probe_config.guest_port
            ),
            "-device".to_string(),
            format!("{},netdev=net0", probe_config.nic_model),
            "-qmp".to_string(),
            format!("unix:{},server=on,wait=off", qmp_socket.to_string_lossy()),
        ]);
        // Stop flag shared with the probe thread's poll loops: the runner
        // stores `true` when the case is over (QEMU failure, timeout, or
        // the guard's Drop), so the probe aborts on its next poll instead
        // of waiting out its deadline.
        let stop = Arc::new(AtomicBool::new(false));
        // The probe asset owns the concrete test content (fixtures such as
        // `vm-memory.toml` and the assertions) in the case directory; the
        // guard stays orchestration-only.
        let probe_addr = format!("127.0.0.1:{host_port}");
        let probe_stop = stop.clone();
        let builtin_configs = qemu
            .boot
            .initramfs
            .as_ref()
            .map(|archive| Path::new(archive).with_extension("configs"));
        let probe: host_probe::HostHttpProbeFn = Box::new(move || {
            let mut output = Vec::new();
            if let Some((observed, deadline)) = observation {
                while !observed.matched_before(deadline) {
                    if probe_stop.load(std::sync::atomic::Ordering::Acquire)
                        || std::time::Instant::now() >= deadline
                    {
                        return host_probe::HostHttpProbeOutcome::failed(anyhow::anyhow!(
                            "required serial boot observation did not complete"
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
            for (config, directory, timeout) in probes {
                let outcome = super::http_probe::run_bounded(
                    &probe_addr,
                    &config,
                    &directory,
                    builtin_configs.as_deref(),
                    probe_stop.clone(),
                    timeout
                        .filter(|seconds| *seconds > 0)
                        .map(std::time::Duration::from_secs),
                );
                output.extend(outcome.output);
                if let Err(error) = outcome.verdict {
                    return host_probe::HostHttpProbeOutcome {
                        output,
                        verdict: Err(
                            error.context(format!("host check {} failed", directory.display()))
                        ),
                    };
                }
            }
            host_probe::HostHttpProbeOutcome {
                output,
                verdict: Ok(()),
            }
        });
        host_probe_guard = Some(host_probe::HostHttpProbeGuard::start(
            &probe_config,
            host_port,
            &case.case.name,
            Some(qmp_socket),
            stop,
            probe,
        )?);
    }

    Ok(HostChecks {
        guard: host_probe_guard,
        capture: observation_capture,
        _qmp_directory: qmp_directory,
    })
}

fn pick_free_local_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("failed to pick a free local port for QEMU hostfwd")?;
    Ok(listener.local_addr()?.port())
}

/// Parse the optional `[host_http_probe]` section from an Axvisor qemu case
/// config. The section is axvisor-specific, so it is read directly from the
/// case toml here instead of going through the generic
/// [`crate::execution::qemu::load_qemu_case_extra_config`] (which no longer carries the
/// field).
pub(super) fn load_axvisor_http_probe_config(
    qemu_config_path: &Path,
) -> anyhow::Result<Option<AxvisorHttpProbeConfig>> {
    #[derive(Deserialize)]
    struct ProbeSection {
        #[serde(default)]
        host_http_probe: Option<AxvisorHttpProbeConfig>,
    }

    let content = std::fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    Ok(toml::from_str::<ProbeSection>(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))?
        .host_http_probe)
}
