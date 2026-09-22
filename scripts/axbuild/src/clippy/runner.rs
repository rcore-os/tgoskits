use std::{collections::HashMap, io::Write, path::Path, process::Command, sync::Mutex, thread};

use anyhow::{Context, bail};

use super::{
    check::ClippyCheck,
    report::{ClippyRunReport, planned_clippy_report, print_clippy_check_plan},
};
use crate::support::process::run_cargo_status_with_env;

pub(super) fn run_clippy_checks<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    target_dir: &Path,
    checks: &[ClippyCheck],
) -> anyhow::Result<ClippyRunReport> {
    let mut report = planned_clippy_report(checks);
    let package_indexes = report
        .packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.package.clone(), index))
        .collect::<HashMap<_, _>>();

    for (index, check) in checks.iter().enumerate() {
        print_clippy_check_plan(workspace_root, index, checks.len(), check);

        let package_index = package_indexes[check.package.as_str()];
        let package_report = &mut report.packages[package_index];
        package_report.total_checks += 1;

        let report_session = if check
            .target
            .as_deref()
            .is_some_and(|target| target.starts_with("aarch64-"))
        {
            Some(crate::build::start_future_incompat_report_session(
                target_dir,
            )?)
        } else {
            None
        };
        let cargo_result = runner.run_clippy(workspace_root, target_dir, check);
        let success =
            crate::build::finish_future_incompat_report_status(report_session, cargo_result)?;

        if success {
            report.passed_checks += 1;
            println!("ok: {}", check.label());
        } else {
            package_report.failed_checks.push(check.label());
            bail!(
                "clippy failed for {}: aborting (fail-fast, {} check(s) remaining)",
                check.label(),
                checks.len() - index - 1
            );
        }
    }

    Ok(report)
}

struct Dispatch {
    next: usize,
    stopped: bool,
}

type CheckOutput = (bool, Vec<u8>, Vec<u8>);

pub(super) fn run_parallel_clippy_checks(
    workspace_root: &Path,
    target_dir: &Path,
    checks: &[ClippyCheck],
    jobs: usize,
) -> anyhow::Result<ClippyRunReport> {
    run_parallel_checks(
        workspace_root,
        target_dir,
        checks,
        jobs,
        |root, target, check| {
            let invocation = check.cargo_invocation();
            let mut command = Command::new("cargo");
            command
                .current_dir(root)
                .args(&invocation.args)
                .envs(invocation.env.iter().map(|(key, value)| (key, value)));
            command.env("CARGO_TARGET_DIR", target);
            let output = command
                .output()
                .with_context(|| format!("failed to spawn cargo clippy for {}", check.label()))?;
            Ok((output.status.success(), output.stdout, output.stderr))
        },
    )
}

pub(super) fn run_parallel_checks<F>(
    workspace_root: &Path,
    target_dir: &Path,
    checks: &[ClippyCheck],
    jobs: usize,
    run: F,
) -> anyhow::Result<ClippyRunReport>
where
    F: Fn(&Path, &Path, &ClippyCheck) -> anyhow::Result<CheckOutput> + Sync,
{
    let dispatch = Mutex::new(Dispatch {
        next: 0,
        stopped: false,
    });
    let mut completed = thread::scope(|scope| {
        let workers = (0..jobs.min(checks.len()))
            .map(|worker| {
                let dispatch = &dispatch;
                let run = &run;
                scope.spawn(move || {
                    let mut results = Vec::new();
                    let worker_dir = target_dir
                        .join("clippy-workers")
                        .join(format!("worker-{worker}"));
                    loop {
                        let index = {
                            let mut state = dispatch.lock().expect("clippy dispatch lock poisoned");
                            if state.stopped || state.next == checks.len() {
                                break;
                            }
                            let index = state.next;
                            state.next += 1;
                            index
                        };
                        let check = &checks[index];
                        let result = (|| {
                            let session = if check
                                .target
                                .as_deref()
                                .is_some_and(|target| target.starts_with("aarch64-"))
                            {
                                Some(crate::build::start_future_incompat_report_session(
                                    &worker_dir,
                                )?)
                            } else {
                                None
                            };
                            let output = run(workspace_root, &worker_dir, check);
                            let success = output.as_ref().map(|output| output.0);
                            let accepted = crate::build::finish_future_incompat_report_status(
                                session,
                                success.map_err(|error| anyhow::anyhow!("{error:#}")),
                            );
                            match output {
                                Ok((_, stdout, mut stderr)) => match accepted {
                                    Ok(success) => Ok((success, stdout, stderr)),
                                    Err(error) => {
                                        stderr.extend_from_slice(format!("{error:#}\n").as_bytes());
                                        Ok((false, stdout, stderr))
                                    }
                                },
                                Err(error) => Err(accepted.err().unwrap_or(error)),
                            }
                        })();
                        if !matches!(result, Ok((true, _, _))) {
                            dispatch
                                .lock()
                                .expect("clippy dispatch lock poisoned")
                                .stopped = true;
                        }
                        results.push((index, result));
                    }
                    results
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("clippy worker panicked"))
            .collect::<Vec<_>>()
    });
    completed.sort_by_key(|(index, _)| *index);

    let mut report = planned_clippy_report(checks);
    let package_indexes = report
        .packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.package.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut errors = Vec::new();
    for (index, result) in completed {
        let check = &checks[index];
        print_clippy_check_plan(workspace_root, index, checks.len(), check);
        std::io::stdout().flush()?;
        let package = &mut report.packages[package_indexes[check.package.as_str()]];
        package.total_checks += 1;
        match result {
            Ok((success, stdout, stderr)) => {
                std::io::stdout().write_all(&stdout)?;
                std::io::stderr().write_all(&stderr)?;
                if success {
                    report.passed_checks += 1;
                    println!("ok: {}", check.label());
                } else {
                    package.failed_checks.push(check.label());
                }
            }
            Err(error) => {
                package.failed_checks.push(check.label());
                errors.push(format!("{}: {error:#}", check.label()));
            }
        }
    }
    if !errors.is_empty() {
        eprintln!("clippy execution errors: {}", errors.join("; "));
    }
    Ok(report)
}

pub(super) trait CargoRunner {
    fn run_clippy(
        &mut self,
        workspace_root: &Path,
        target_dir: &Path,
        check: &ClippyCheck,
    ) -> anyhow::Result<bool>;
}

pub(super) struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_clippy(
        &mut self,
        workspace_root: &Path,
        target_dir: &Path,
        check: &ClippyCheck,
    ) -> anyhow::Result<bool> {
        let invocation = check.cargo_invocation();
        let mut env = invocation.env;
        let target_dir = target_dir.display().to_string();
        if let Some((_, value)) = env.iter_mut().find(|(key, _)| key == "CARGO_TARGET_DIR") {
            *value = target_dir;
        } else {
            env.push(("CARGO_TARGET_DIR".to_string(), target_dir));
        }
        run_cargo_status_with_env(workspace_root, &invocation.args, &env)
    }
}
