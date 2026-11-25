//! AxVisor checks retain member boundaries over one QEMU lifetime.
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use ostool::run::{ShellCheckStep, qemu::QemuConfig};
use serde::Deserialize;

use super::{
    AxvisorQemuCase,
    host_probe::{self, HostHttpProbeOutcome},
    http_probe,
    types::AxvisorHttpProbeConfig,
};
use crate::execution::qemu::checks::{QemuChecks, member_deadline};

pub(super) struct HostChecks(Option<QemuChecks>);
impl HostChecks {
    pub(super) fn is_configured(&self) -> bool {
        self.0.is_some()
    }
    pub(super) fn finish(self) -> Option<HostHttpProbeOutcome> {
        self.0.map(|guard| match guard.finish() {
            Ok(output) => HostHttpProbeOutcome {
                output,
                verdict: Ok(()),
            },
            Err(error) => HostHttpProbeOutcome::failed(error),
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ForwardingEndpoint {
    guest_port: u16,
    nic_model: String,
}
impl ForwardingEndpoint {
    fn from_config(config: &AxvisorHttpProbeConfig) -> Result<Self> {
        ensure!(
            config.guest_port != 0 && !config.nic_model.trim().is_empty(),
            "HTTP endpoint requires a port and NIC model"
        );
        Ok(Self {
            guest_port: config.guest_port,
            nic_model: config.nic_model.clone(),
        })
    }
    fn compatible(&self, config: &AxvisorHttpProbeConfig, path: &Path) -> Result<()> {
        let other = Self::from_config(config)?;
        ensure!(
            *self == other,
            "incompatible HTTP endpoint in {}: expected port={} nic={}, got port={} nic={}",
            path.display(),
            self.guest_port,
            self.nic_model,
            other.guest_port,
            other.nic_model
        );
        Ok(())
    }
    fn attach(&self, qemu: &mut QemuConfig) -> Result<String> {
        let port = pick_free_local_port()?;
        qemu.args.extend([
            "-netdev".into(),
            format!(
                "user,id=net0,hostfwd=tcp:127.0.0.1:{port}-:{}",
                self.guest_port
            ),
            "-device".into(),
            format!("{},netdev=net0", self.nic_model),
        ]);
        Ok(format!("127.0.0.1:{port}"))
    }
}

struct Member {
    path: PathBuf,
    prerequisite: bool,
    budget: Duration,
    steps: Vec<ShellCheckStep>,
    probe: Option<AxvisorHttpProbeConfig>,
}

pub(super) fn prepare(qemu: &mut QemuConfig, case: &AxvisorQemuCase) -> Result<HostChecks> {
    let plan = super::checks::load_selected_runtime_checks(
        &case.case.qemu_config_path,
        case.selected_checks.as_deref(),
    )?;
    let inline = load_axvisor_http_probe_config(&case.case.qemu_config_path)?;
    if plan.checks.is_empty() && inline.is_none() {
        return Ok(HostChecks(None));
    }
    let mut members = Vec::new();
    let mut resolved = qemu.shell_check_steps.iter();
    if plan.checks.is_empty() {
        members.push(Member {
            path: case.case.qemu_config_path.clone(),
            prerequisite: false,
            budget: Duration::from_secs(
                qemu.timeout
                    .filter(|s| *s > 0)
                    .context("HTTP check requires a finite timeout")?,
            ),
            steps: resolved.by_ref().cloned().collect(),
            probe: inline,
        });
    } else {
        for check in plan.checks {
            let seconds = check
                .document
                .get("timeout")
                .map(|v| v.as_integer().context("member timeout must be an integer"))
                .transpose()?
                .map(u64::try_from)
                .transpose()?
                .or(qemu.timeout)
                .context("check requires a finite timeout")?;
            ensure!(
                seconds > 0,
                "member timeout must be positive: {}",
                check.path.display()
            );
            let count = check
                .document
                .get("shell_check_steps")
                .and_then(toml::Value::as_array)
                .map_or(0, Vec::len);
            let steps: Vec<_> = resolved.by_ref().take(count).cloned().collect();
            ensure!(
                steps.len() == count,
                "resolved steps do not match member {}",
                check.path.display()
            );
            members.push(Member {
                probe: load_axvisor_http_probe_config(&check.path)?,
                path: check.path,
                prerequisite: check.prerequisite,
                budget: Duration::from_secs(seconds),
                steps,
            });
        }
    }
    ensure!(resolved.next().is_none(), "unclaimed serial steps");
    let mut endpoint: Option<(ForwardingEndpoint, PathBuf)> = None;
    for member in &members {
        if let Some(config) = &member.probe {
            ensure!(
                config.connect_timeout_secs > 0 && config.request_timeout_secs > 0,
                "HTTP timeouts must be positive in {}",
                member.path.display()
            );
            if let Some((endpoint, first)) = &endpoint {
                endpoint
                    .compatible(config, &member.path)
                    .with_context(|| format!("first HTTP member {}", first.display()))?;
            } else {
                endpoint = Some((
                    ForwardingEndpoint::from_config(config)?,
                    member.path.clone(),
                ));
            }
        }
    }
    let address = endpoint.map(|(e, _)| e.attach(qemu)).transpose()?;
    let builtin = qemu
        .boot
        .initramfs
        .as_ref()
        .map(|p| PathBuf::from(p).with_extension("configs"));
    let guard = QemuChecks::start(qemu, move |serial, group_deadline, stop| {
        let mut output = Vec::new();
        for (index, member) in members.into_iter().enumerate() {
            let start = if index == 0 {
                serial.connected_at()?
            } else {
                Instant::now()
            };
            let deadline = member_deadline(start, member.budget, group_deadline)?;
            println!(
                "[axbuild] {}: {} budget={:?}",
                if member.prerequisite {
                    "prerequisite"
                } else {
                    "member check"
                },
                member.path.display(),
                member.budget
            );
            let result = (|| -> Result<()> {
                serial.run_until(&member.steps, deadline)?;
                if let Some(config) = member.probe {
                    let address = address
                        .as_deref()
                        .context("HTTP member has no forwarding endpoint")?;
                    host_probe::wait_for_port_ready_until(
                        address,
                        Duration::from_secs(config.connect_timeout_secs),
                        deadline,
                        &stop,
                    )?;
                    let outcome = http_probe::run_until(
                        address,
                        &config,
                        member.path.parent().context("member has no directory")?,
                        builtin.as_deref(),
                        stop.clone(),
                        Some(deadline),
                    );
                    output.extend(outcome.output);
                    outcome.verdict?;
                }
                serial.verdict()
            })();
            if let Err(error) = result {
                // Replay diagnostics before returning the member failure.
                use std::io::Write;
                std::io::stdout().write_all(&output)?;
                return Err(error.context(format!("check {}", member.path.display())));
            }
        }
        Ok(output)
    })?;
    Ok(HostChecks(Some(guard)))
}

pub(super) fn pick_free_local_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("pick HTTP forwarding port")?
        .local_addr()?
        .port())
}

pub(super) fn load_axvisor_http_probe_config(
    path: &Path,
) -> Result<Option<AxvisorHttpProbeConfig>> {
    #[derive(Deserialize)]
    struct Section {
        #[serde(default)]
        host_http_probe: Option<AxvisorHttpProbeConfig>,
    }
    Ok(toml::from_str::<Section>(&std::fs::read_to_string(path)?)
        .with_context(|| format!("parse HTTP check {}", path.display()))?
        .host_http_probe)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_members_share_an_endpoint_but_keep_their_request_budgets() {
        let read = |text| toml::from_str::<AxvisorHttpProbeConfig>(text).unwrap();
        let first = read("guest_port=8080\nnic_model='virtio-net-pci'\nrequest_timeout_secs=5");
        let endpoint = ForwardingEndpoint::from_config(&first).unwrap();
        let same = read("guest_port=8080\nnic_model='virtio-net-pci'\nrequest_timeout_secs=30");
        endpoint
            .compatible(&same, Path::new("second.toml"))
            .unwrap();
        for text in ["guest_port=9090", "nic_model='e1000'", "guest_port=0"] {
            assert!(
                endpoint
                    .compatible(&read(text), Path::new("conflict.toml"))
                    .is_err()
            );
        }
    }
}

#[cfg(test)]
mod endpoint_batches {
    use super::*;
    use crate::execution::case::{GroupedCommandSelection, TestQemuCase};
    #[test]
    fn incompatible_member_endpoints_are_rejected_before_any_runtime_is_started() {
        for interactive in [false, true] {
            for other in ["guest_port=9090", "nic_model='e1000'"] {
                let root = tempfile::tempdir().unwrap();
                let path = root.path().join("qemu-aarch64.toml");
                std::fs::write(
                    &path,
                    "args=[]\ntimeout=20\n[axbuild]\nchecks=['first.toml','second.toml']",
                )
                .unwrap();
                let serial = if interactive {
                    "[[shell_check_steps]]\nshell_prefix='>'\nshell_cmd='ping'\nsuccess_regex=['OK'\
                     ]\n"
                } else {
                    ""
                };
                std::fs::write(
                    root.path().join("first.toml"),
                    format!("timeout=10\n{serial}[host_http_probe]\nguest_port=8080"),
                )
                .unwrap();
                std::fs::write(
                    root.path().join("second.toml"),
                    format!("timeout=10\n[host_http_probe]\n{other}"),
                )
                .unwrap();
                let case = AxvisorQemuCase {
                    selected_checks: None,
                    build_group: "fixture".into(),
                    build_config_path: root.path().join("build.toml"),
                    case: TestQemuCase {
                        name: "fixture".into(),
                        display_name: "fixture".into(),
                        case_dir: root.path().into(),
                        qemu_config_path: path.clone(),
                        test_commands: vec![],
                        grouped_command_selection: GroupedCommandSelection::default(),
                        host_http_server: None,
                        subcases: vec![],
                        grouped_subcase_filter: None,
                        ltp_case_id: None,
                    },
                };
                let plan = super::super::checks::load_runtime_checks(&path).unwrap();
                let steps = plan
                    .runtime
                    .get("shell_check_steps")
                    .map(|v| v.clone().try_into().unwrap())
                    .unwrap_or_default();
                let mut qemu = QemuConfig {
                    timeout: Some(20),
                    shell_check_steps: steps,
                    ..Default::default()
                };
                let error = match prepare(&mut qemu, &case) {
                    Ok(_) => panic!("conflicting endpoints accepted"),
                    Err(error) => error,
                };
                assert!(
                    format!("{error:#}").contains("incompatible HTTP endpoint"),
                    "{error:#}"
                );
                assert!(
                    qemu.args.is_empty(),
                    "endpoint mismatch must fail before attaching runtime transports"
                );
            }
        }
    }
}
