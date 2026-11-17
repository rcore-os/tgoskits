//! A serial and HTTP sequence owns one QEMU lifetime and one final verdict.

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use anyhow::{Context, ensure};
use ostool::run::{ShellCheckStep, qemu::QemuConfig};

use super::{
    AxvisorQemuCase,
    host_checks::{load_axvisor_http_probe_config, pick_free_local_port},
    host_probe::{HostHttpProbeGuard, HostHttpProbeOutcome},
    http_probe,
    serial_checks::SerialMonitor,
    types::AxvisorHttpProbeConfig,
};
use crate::execution::case::RuntimeChecks;

enum Phase {
    Serial {
        name: String,
        steps: Vec<ShellCheckStep>,
        budget: Duration,
    },
    Http {
        config: AxvisorHttpProbeConfig,
        directory: PathBuf,
        budget: Duration,
    },
}

pub(super) struct MixedChecks {
    guard: HostHttpProbeGuard,
    serial: SerialMonitor,
    _directory: tempfile::TempDir,
}

impl MixedChecks {
    pub(super) fn finish(self) -> HostHttpProbeOutcome {
        let mut outcome = self.guard.finish().unwrap_or_else(|| {
            HostHttpProbeOutcome::failed(anyhow::anyhow!("mixed check sequence did not finish"))
        });
        // Keep serial failure monitoring alive until QEMU has actually exited,
        // including the interval between the final HTTP assertion and QMP quit.
        if let Err(error) = self.serial.finish() {
            outcome.verdict = Err(error);
        }
        outcome
    }
}

pub(super) fn prepare(
    qemu: &mut QemuConfig,
    case: &AxvisorQemuCase,
    plan: RuntimeChecks,
) -> anyhow::Result<MixedChecks> {
    let mut phases = Vec::new();
    let mut endpoint: Option<AxvisorHttpProbeConfig> = None;
    let mut resolved_steps = qemu.shell_check_steps.iter();
    for check in plan.checks {
        let seconds = check
            .document
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .map(u64::try_from)
            .transpose()?
            .or(qemu.timeout)
            .context("mixed check requires a finite timeout")?;
        ensure!(seconds > 0, "mixed check timeout must be positive");
        let budget = Duration::from_secs(seconds);
        if let Some(steps) = check.document.get("shell_check_steps") {
            let count = steps
                .as_array()
                .context("shell_check_steps must be an array")?
                .len();
            let steps: Vec<_> = resolved_steps.by_ref().take(count).cloned().collect();
            ensure!(
                steps.len() == count,
                "resolved serial steps do not match the check sequence"
            );
            phases.push(Phase::Serial {
                name: check.path.display().to_string(),
                steps,
                budget,
            });
        }
        if let Some(config) = load_axvisor_http_probe_config(&check.path)? {
            if let Some(endpoint) = &endpoint {
                ensure!(
                    endpoint.guest_port == config.guest_port
                        && endpoint.nic_model == config.nic_model,
                    "one boot cannot use different HTTP forwarding endpoints"
                );
            } else {
                endpoint = Some(config.clone());
            }
            phases.push(Phase::Http {
                config,
                directory: check
                    .path
                    .parent()
                    .context("check has no parent")?
                    .to_path_buf(),
                budget,
            });
        }
    }
    ensure!(
        resolved_steps.next().is_none(),
        "unclaimed resolved serial steps"
    );
    let endpoint = endpoint.context("mixed check sequence has no HTTP endpoint")?;
    let host_port = pick_free_local_port()?;
    let directory = tempfile::tempdir()?;
    let serial_socket = directory.path().join("serial.sock");
    let qmp_socket = directory.path().join("qmp.sock");
    let serial_argument = format!("unix:{},server=on,wait=on", serial_socket.display());
    if let Some(index) = qemu.args.iter().position(|arg| arg == "-serial") {
        *qemu
            .args
            .get_mut(index + 1)
            .context("-serial has no argument")? = serial_argument;
    } else {
        qemu.args.extend(["-serial".into(), serial_argument]);
    }
    qemu.args.extend([
        "-netdev".into(),
        format!(
            "user,id=net0,hostfwd=tcp:127.0.0.1:{host_port}-:{}",
            endpoint.guest_port
        ),
        "-device".into(),
        format!("{},netdev=net0", endpoint.nic_model),
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", qmp_socket.display()),
    ]);
    let stop = Arc::new(AtomicBool::new(false));
    let (serial, mut checks) = SerialMonitor::start(
        &serial_socket,
        &qemu.fail_regex,
        stop.clone(),
        Duration::from_secs(endpoint.connect_timeout_secs),
        std::io::stdout(),
    )?;
    // The socket driver executes these original steps. Leaving a second driver
    // in ostool would either send commands twice or terminate QEMU prematurely.
    qemu.shell_check_steps.clear();
    let builtin = qemu
        .boot
        .initramfs
        .as_ref()
        .map(|path| PathBuf::from(path).with_extension("configs"));
    let address = format!("127.0.0.1:{host_port}");
    let probe_stop = stop.clone();
    let probe = Box::new(move || {
        let mut output = Vec::new();
        let result = (|| -> anyhow::Result<()> {
            for phase in phases {
                checks.verdict()?;
                match phase {
                    Phase::Serial {
                        name,
                        steps,
                        budget,
                    } => {
                        println!("[axbuild] serial check: {name} steps={}", steps.len());
                        checks
                            .run(&steps, budget)
                            .with_context(|| format!("serial check {name}"))?;
                    }
                    Phase::Http {
                        config,
                        directory,
                        budget,
                    } => {
                        super::host_probe::wait_for_port_ready(
                            &address,
                            Duration::from_secs(config.connect_timeout_secs),
                            &probe_stop,
                        )?;
                        let outcome = http_probe::run_bounded(
                            &address,
                            &config,
                            &directory,
                            builtin.as_deref(),
                            probe_stop.clone(),
                            Some(budget),
                        );
                        output.extend(outcome.output);
                        checks.verdict()?;
                        outcome
                            .verdict
                            .with_context(|| format!("HTTP check {}", directory.display()))?;
                    }
                }
            }
            checks.verdict()
        })();
        HostHttpProbeOutcome {
            output,
            verdict: result,
        }
    });
    let guard = HostHttpProbeGuard::start_ordered(
        &endpoint,
        host_port,
        &case.case.name,
        Some(qmp_socket),
        stop,
        probe,
    )?;
    Ok(MixedChecks {
        guard,
        serial,
        _directory: directory,
    })
}
