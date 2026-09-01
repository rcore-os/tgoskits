use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use regex::Regex;
use serde::Deserialize;

const FUTURE_INCOMPAT_REPORT: &str = ".future-incompat-report.json";
const FUTURE_INCOMPAT_HISTORY: &str = ".future-incompat-report.json.axbuild-history";
const FUTURE_INCOMPAT_LOCK: &str = ".future-incompat-report.json.axbuild-lock";
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const MAX_REPORTS: usize = 5;
const REPORT_VERSION: u64 = 0;
const RUST_ISSUE: &str = "https://github.com/rust-lang/rust/issues/134375";
const WARNING: &str = "warning: enabling the `neon` target feature on the current target is \
                       unsound due to ABI issues";
const FUTURE_WARNING: &str = "warning: this was previously accepted by the compiler but is being \
                              phased out; it will become a hard error in a future release!";
const ISSUE_NOTE: &str = "note: for more information, see issue #134375 <https://github.com/rust-lang/rust/issues/134375>";
const ALLOWED_PACKAGES: &[&str] = &["core@0.0.0", "memchr@2.8.3"];
const CORE_CONTEXT_NOTES: &[&str] = &[
    "note: this warning originates in the macro `impl_internal_sve_predicate` (in Nightly builds, \
     run with -Z macro-backtrace for more info)",
    "note: this warning originates in the macro `impl_sign_conversions_sv` (in Nightly builds, \
     run with -Z macro-backtrace for more info)",
    "note: this warning originates in the macro `impl_sign_conversions` (in Nightly builds, run \
     with -Z macro-backtrace for more info)",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FutureIncompatReportFile {
    version: u64,
    next_id: u64,
    reports: Vec<FutureIncompatReport>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FutureIncompatReport {
    id: u64,
    suggestion_message: String,
    per_package: BTreeMap<String, String>,
}

/// Isolates Cargo's persistent report so validation observes one invocation only.
#[derive(Debug)]
pub(crate) struct FutureIncompatReportSession {
    report_path: PathBuf,
    history_path: PathBuf,
    lock: File,
    finished: bool,
}

pub(crate) fn cargo_target_dir_for(
    workspace_root: &Path,
    cargo_args: &[String],
) -> anyhow::Result<PathBuf> {
    let mut target_dir = None;
    let mut args = cargo_args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--target-dir" {
            let value = args
                .next()
                .context("Cargo argument `--target-dir` is missing its value")?;
            target_dir = Some(PathBuf::from(value));
        } else if let Some(value) = arg.strip_prefix("--target-dir=") {
            if value.is_empty() {
                bail!("Cargo argument `--target-dir=` is missing its value");
            }
            target_dir = Some(PathBuf::from(value));
        }
    }

    let target_dir = target_dir.unwrap_or_else(|| workspace_root.join("target"));
    Ok(if target_dir.is_absolute() {
        target_dir
    } else {
        workspace_root.join(target_dir)
    })
}

pub(crate) fn start_future_incompat_report_session(
    target_dir: &Path,
) -> anyhow::Result<FutureIncompatReportSession> {
    fs::create_dir_all(target_dir).with_context(|| {
        format!(
            "failed to create Cargo target directory {}",
            target_dir.display()
        )
    })?;
    let lock_path = target_dir.join(FUTURE_INCOMPAT_LOCK);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    lock_report_file(&lock, &lock_path)?;

    let report_path = target_dir.join(FUTURE_INCOMPAT_REPORT);
    let history_path = target_dir.join(FUTURE_INCOMPAT_HISTORY);
    recover_abandoned_history(&report_path, &history_path)?;
    if report_path.exists() {
        let content = fs::read_to_string(&report_path)
            .with_context(|| format!("failed to read {}", report_path.display()))?;
        parse_report_json(&content)
            .and_then(|report| validate_report_structure(&report))
            .with_context(|| {
                format!(
                    "existing Cargo future-incompatibility report {} is damaged or unsupported",
                    report_path.display()
                )
            })?;
        fs::rename(&report_path, &history_path).with_context(|| {
            format!(
                "failed to isolate existing Cargo future-incompatibility report {}",
                report_path.display()
            )
        })?;
    }

    Ok(FutureIncompatReportSession {
        report_path,
        history_path,
        lock,
        finished: false,
    })
}

pub(crate) fn finish_future_incompat_report_session<T>(
    session: Option<FutureIncompatReportSession>,
    cargo_result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    let cargo_succeeded = cargo_result.is_ok();
    match session {
        Some(session) => session.finish(cargo_result, cargo_succeeded),
        None => cargo_result,
    }
}

pub(crate) fn finish_future_incompat_report_status(
    session: Option<FutureIncompatReportSession>,
    cargo_result: anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let cargo_succeeded = matches!(&cargo_result, Ok(true));
    match session {
        Some(session) => session.finish(cargo_result, cargo_succeeded),
        None => cargo_result,
    }
}

fn lock_report_file(lock: &File, path: &Path) -> anyhow::Result<()> {
    let started = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) if started.elapsed() < LOCK_TIMEOUT => {
                thread::sleep(LOCK_RETRY_INTERVAL);
            }
            Err(TryLockError::WouldBlock) => {
                bail!(
                    "timed out waiting for Cargo future-incompatibility report lock {}",
                    path.display()
                );
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("failed to lock {}", path.display()));
            }
        }
    }
}

fn recover_abandoned_history(report_path: &Path, history_path: &Path) -> anyhow::Result<()> {
    if !history_path.exists() {
        return Ok(());
    }
    if report_path.exists() {
        bail!(
            "both Cargo future-incompatibility report {} and interrupted axbuild history {} exist",
            report_path.display(),
            history_path.display()
        );
    }
    fs::rename(history_path, report_path).with_context(|| {
        format!(
            "failed to recover interrupted Cargo future-incompatibility report {}",
            history_path.display()
        )
    })
}

impl FutureIncompatReportSession {
    fn finish<T>(
        mut self,
        cargo_result: anyhow::Result<T>,
        cargo_succeeded: bool,
    ) -> anyhow::Result<T> {
        let validation = if cargo_succeeded {
            self.validate_current_report()
        } else {
            Ok(())
        };
        let restoration = self.restore_history();
        let unlock = self
            .lock
            .unlock()
            .with_context(|| "failed to unlock Cargo future-incompatibility report");
        self.finished = true;
        let report_result = validation.and(restoration).and(unlock);

        match (cargo_result, report_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(report_error)) => Err(error.context(format!(
                "also failed to restore Cargo future-incompatibility report: {report_error:#}"
            ))),
        }
    }

    fn validate_current_report(&self) -> anyhow::Result<()> {
        let packages = validate_report_path(&self.report_path)?;
        if !packages.is_empty() {
            println!(
                "[axbuild] accepted known upstream Rust #134375 future-incompatibility for {}",
                packages.join(", ")
            );
        }
        Ok(())
    }

    fn restore_history(&self) -> anyhow::Result<()> {
        // Restore Cargo's exact pre-invocation state. Rewriting its private report history here
        // would couple axbuild to Cargo's deduplication and retention implementation.
        match fs::remove_file(&self.report_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove {}", self.report_path.display()));
            }
        }
        if self.history_path.exists() {
            fs::rename(&self.history_path, &self.report_path).with_context(|| {
                format!(
                    "failed to restore Cargo future-incompatibility report {}",
                    self.report_path.display()
                )
            })?;
        }
        Ok(())
    }
}

impl Drop for FutureIncompatReportSession {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.restore_history();
        let _ = self.lock.unlock();
        self.finished = true;
    }
}

fn validate_report_path(path: &Path) -> anyhow::Result<Vec<String>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    validate_report_json(&content).with_context(|| {
        format!(
            "AArch64 future-incompatibility report {} is not an approved Rust #134375 exception",
            path.display()
        )
    })
}

fn validate_report_json(content: &str) -> anyhow::Result<Vec<String>> {
    let report = parse_report_json(content)?;
    validate_report_structure(&report)?;
    if report.reports.is_empty() {
        bail!("Cargo future-incompatibility report contains no diagnostics");
    }

    let mut packages = Vec::new();
    for entry in report.reports {
        if entry.per_package.is_empty() {
            bail!(
                "Cargo future-incompatibility report {} contains no package diagnostics",
                entry.id
            );
        }
        for (package, diagnostic) in entry.per_package {
            if !ALLOWED_PACKAGES.contains(&package.as_str()) {
                bail!("unapproved future-incompatible package `{package}`");
            }
            validate_package_diagnostics(&package, &diagnostic)?;
            if !entry.suggestion_message.contains(&package) {
                bail!(
                    "Cargo future-incompatibility report {} omits `{package}` from its suggestion",
                    entry.id
                );
            }
            packages.push(package);
        }
    }
    packages.sort();
    packages.dedup();
    Ok(packages)
}

fn parse_report_json(content: &str) -> anyhow::Result<FutureIncompatReportFile> {
    serde_json::from_str(content).context("invalid Cargo future-incompatibility JSON")
}

fn validate_report_structure(report: &FutureIncompatReportFile) -> anyhow::Result<()> {
    if report.version != REPORT_VERSION {
        bail!(
            "unsupported Cargo future-incompatibility report version {}",
            report.version
        );
    }

    if report.reports.len() > MAX_REPORTS {
        bail!("Cargo future-incompatibility report retains more than {MAX_REPORTS} entries");
    }
    let report_ids = report
        .reports
        .iter()
        .map(|report| report.id)
        .collect::<Vec<_>>();
    if report_ids.first() == Some(&0) || report_ids.windows(2).any(|ids| ids[0] >= ids[1]) {
        bail!("invalid Cargo future-incompatibility report ids: {report_ids:?}");
    }
    let expected_next_id = report_ids
        .last()
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .context("Cargo future-incompatibility report id overflow")?;
    if report.next_id != expected_next_id {
        bail!(
            "invalid Cargo future-incompatibility report ids: expected next_id \
             {expected_next_id}, found {}",
            report.next_id
        );
    }

    Ok(())
}

fn validate_package_diagnostics(package: &str, diagnostic: &str) -> anyhow::Result<()> {
    let diagnostic = strip_ansi(diagnostic);
    let mut lines = diagnostic.lines();
    let header = lines
        .next()
        .with_context(|| format!("`{package}` has an empty diagnostic body"))?;
    let package_header = format!("The package `{}", package.replace('@', " v"));
    if !header.starts_with(&package_header)
        || !header.ends_with("` currently triggers the following future incompatibility lints:")
    {
        bail!("`{package}` has an unapproved Cargo diagnostic header");
    }

    let lines = lines.collect::<Vec<_>>();
    let mut warning_count = 0;
    let mut index = 0;
    while index < lines.len() {
        if lines[index] != format!("> {WARNING}") {
            bail!(
                "`{package}` contains unapproved content outside a Rust #134375 diagnostic: {}",
                lines[index]
            );
        }
        warning_count += 1;
        let block_start = index;
        index += 1;
        while index < lines.len() && lines[index] != "> " {
            let diagnostic_line = lines[index]
                .strip_prefix('>')
                .unwrap_or(lines[index])
                .trim_start();
            if diagnostic_line.starts_with("warning:")
                || diagnostic_line.starts_with("error:")
                || diagnostic_line.starts_with("note:")
                || diagnostic_line.starts_with("help:")
            {
                bail!(
                    "`{package}` contains an unapproved nested diagnostic: {}",
                    lines[index]
                );
            }
            if let Some(nested) = diagnostic_line.strip_prefix("= ")
                && (nested.starts_with("warning:")
                    || nested.starts_with("error:")
                    || nested.starts_with("note:")
                    || nested.starts_with("help:"))
                && !approved_nested_diagnostic(package, nested)
            {
                bail!(
                    "`{package}` contains an unapproved nested diagnostic: {}",
                    lines[index]
                );
            }
            index += 1;
        }
        if index == lines.len() {
            bail!("`{package}` has a truncated Rust #134375 diagnostic");
        }
        let block = lines[block_start..=index].join("\n");
        if block.matches(FUTURE_WARNING).count() != 1
            || block.matches(ISSUE_NOTE).count() != 1
            || block.matches(RUST_ISSUE).count() != 1
        {
            bail!("`{package}` diagnostic does not match only Rust #134375");
        }
        let remaining_urls = block.replace(RUST_ISSUE, "");
        if remaining_urls.contains("http://") || remaining_urls.contains("https://") {
            bail!("`{package}` diagnostic contains an unapproved issue reference");
        }
        index += 1;
    }

    if warning_count == 0 {
        bail!("`{package}` has no approved Rust #134375 diagnostic");
    }
    Ok(())
}

fn approved_nested_diagnostic(package: &str, diagnostic: &str) -> bool {
    diagnostic == FUTURE_WARNING
        || diagnostic == ISSUE_NOTE
        || (package == "core@0.0.0" && CORE_CONTEXT_NOTES.contains(&diagnostic))
}

fn strip_ansi(input: &str) -> String {
    static ANSI_ESCAPE: OnceLock<Regex> = OnceLock::new();
    ANSI_ESCAPE
        .get_or_init(|| Regex::new("\\x1b\\[[0-9;]*m").expect("valid ANSI escape regex"))
        .replace_all(input, "")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn approved_diagnostic(package: &str) -> String {
        format!(
            "The package `{}` currently triggers the following future incompatibility lints:\n> \
             {WARNING}\n>   --> src/lib.rs:1:1\n>    |\n>    = {FUTURE_WARNING}\n>    = \
             {ISSUE_NOTE}\n> \n",
            package.replace('@', " v")
        )
    }

    fn report_json(version: u64, packages: &[(&str, String)]) -> String {
        let per_package = packages
            .iter()
            .map(|(package, diagnostic)| ((*package).to_string(), diagnostic.clone()))
            .collect::<BTreeMap<_, _>>();
        serde_json::json!({
            "version": version,
            "next_id": 2,
            "reports": [{
                "id": 1,
                "suggestion_message": packages
                    .iter()
                    .map(|(package, _)| *package)
                    .collect::<Vec<_>>()
                    .join(", "),
                "per_package": per_package,
            }],
        })
        .to_string()
    }

    #[test]
    fn missing_report_is_accepted() {
        let path = PathBuf::from("/definitely/missing/future-incompat-report.json");
        assert!(validate_report_path(&path).unwrap().is_empty());
    }

    #[test]
    fn approved_report_accepts_allowed_package_subset() {
        let core_diagnostic = approved_diagnostic("core@0.0.0")
            .replace("> \n", &format!(">    = {}\n> \n", CORE_CONTEXT_NOTES[0]));
        let packages = validate_report_json(&report_json(
            REPORT_VERSION,
            &[
                ("core@0.0.0", core_diagnostic),
                ("memchr@2.8.3", approved_diagnostic("memchr@2.8.3")),
            ],
        ))
        .unwrap();

        assert_eq!(packages, ["core@0.0.0", "memchr@2.8.3"]);
    }

    #[test]
    fn report_rejects_unapproved_package_or_version() {
        let package_error = validate_report_json(&report_json(
            REPORT_VERSION,
            &[("other@1.0.0", approved_diagnostic("other@1.0.0"))],
        ))
        .unwrap_err();
        assert!(package_error.to_string().contains("unapproved"));

        let version_error = validate_report_json(&report_json(
            REPORT_VERSION + 1,
            &[("core@0.0.0", approved_diagnostic("core@0.0.0"))],
        ))
        .unwrap_err();
        assert!(version_error.to_string().contains("unsupported"));
    }

    #[test]
    fn report_rejects_additional_diagnostic_and_malformed_json() {
        let diagnostic = format!(
            "{}> warning: another future incompatibility\n>    = {FUTURE_WARNING}\n>    = \
             {ISSUE_NOTE}\n> \n",
            approved_diagnostic("core@0.0.0")
        );
        let diagnostic_error =
            validate_report_json(&report_json(REPORT_VERSION, &[("core@0.0.0", diagnostic)]))
                .unwrap_err();
        assert!(diagnostic_error.to_string().contains("unapproved"));

        assert!(validate_report_json("{").is_err());
    }

    #[test]
    fn report_rejects_additional_note_and_duplicate_ids() {
        let diagnostic = format!(
            "{}> note: unapproved diagnostic\n",
            approved_diagnostic("core@0.0.0")
        );
        let diagnostic_error =
            validate_report_json(&report_json(REPORT_VERSION, &[("core@0.0.0", diagnostic)]))
                .unwrap_err();
        assert!(diagnostic_error.to_string().contains("unapproved content"));

        let nested_diagnostic = approved_diagnostic("core@0.0.0")
            .replace("> \n", ">    = note: unapproved nested diagnostic\n> \n");
        let nested_error = validate_report_json(&report_json(
            REPORT_VERSION,
            &[("core@0.0.0", nested_diagnostic)],
        ))
        .unwrap_err();
        assert!(
            nested_error
                .to_string()
                .contains("unapproved nested diagnostic")
        );

        let mut duplicate_ids = serde_json::from_str::<serde_json::Value>(&report_json(
            REPORT_VERSION,
            &[("core@0.0.0", approved_diagnostic("core@0.0.0"))],
        ))
        .unwrap();
        let duplicate = duplicate_ids["reports"][0].clone();
        duplicate_ids["reports"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        let id_error = validate_report_json(&duplicate_ids.to_string()).unwrap_err();
        assert!(id_error.to_string().contains("report ids"));
    }

    #[test]
    fn cargo_target_dir_comes_from_the_cargo_invocation_not_artifact_depth() {
        let workspace = Path::new("/workspace");

        assert_eq!(
            cargo_target_dir_for(workspace, &[]).unwrap(),
            Path::new("/workspace/target")
        );
        assert_eq!(
            cargo_target_dir_for(workspace, &["--target-dir".into(), "ktest-target".into()])
                .unwrap(),
            Path::new("/workspace/ktest-target")
        );
        assert_eq!(
            cargo_target_dir_for(workspace, &["--target-dir=/tmp/custom-target".into()]).unwrap(),
            Path::new("/tmp/custom-target")
        );
    }

    #[test]
    fn report_accepts_cargo_history_after_old_entries_are_trimmed() {
        let diagnostic = approved_diagnostic("memchr@2.8.3");
        let history = serde_json::json!({
            "version": REPORT_VERSION,
            "next_id": 8,
            "reports": [{
                "id": 7,
                "suggestion_message": "memchr@2.8.3",
                "per_package": { "memchr@2.8.3": diagnostic },
            }],
        });

        assert_eq!(
            validate_report_json(&history.to_string()).unwrap(),
            ["memchr@2.8.3"]
        );
    }

    #[test]
    fn session_validates_only_the_current_invocation_and_restores_history() {
        let root = tempfile::tempdir().unwrap();
        let report_path = root.path().join(FUTURE_INCOMPAT_REPORT);
        let historical = serde_json::json!({
            "version": REPORT_VERSION,
            "next_id": 8,
            "reports": [{
                "id": 7,
                "suggestion_message": "old@1.0.0",
                "per_package": { "old@1.0.0": "historical diagnostic" },
            }],
        })
        .to_string();
        fs::write(&report_path, &historical).unwrap();

        let session = start_future_incompat_report_session(root.path()).unwrap();
        assert!(!report_path.exists());
        fs::write(
            &report_path,
            report_json(
                REPORT_VERSION,
                &[("memchr@2.8.3", approved_diagnostic("memchr@2.8.3"))],
            ),
        )
        .unwrap();

        session.finish(Ok::<(), anyhow::Error>(()), true).unwrap();
        assert_eq!(fs::read_to_string(report_path).unwrap(), historical);
    }

    #[test]
    fn session_rejects_unapproved_current_report_and_still_restores_history() {
        let root = tempfile::tempdir().unwrap();
        let report_path = root.path().join(FUTURE_INCOMPAT_REPORT);
        let historical = serde_json::json!({
            "version": REPORT_VERSION,
            "next_id": 1,
            "reports": [],
        })
        .to_string();
        fs::write(&report_path, &historical).unwrap();
        let session = start_future_incompat_report_session(root.path()).unwrap();
        fs::write(
            &report_path,
            report_json(
                REPORT_VERSION,
                &[("other@1.0.0", approved_diagnostic("other@1.0.0"))],
            ),
        )
        .unwrap();

        let error = session
            .finish(Ok::<(), anyhow::Error>(()), true)
            .unwrap_err();

        assert!(error.to_string().contains("not an approved Rust #134375"));
        assert_eq!(fs::read_to_string(report_path).unwrap(), historical);
    }

    #[test]
    fn session_rejects_damaged_history_before_running_cargo() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(FUTURE_INCOMPAT_REPORT), "{").unwrap();

        let error = start_future_incompat_report_session(root.path()).unwrap_err();

        assert!(
            format!("{error:#}").contains("invalid Cargo future-incompatibility JSON"),
            "{error:#}"
        );
    }
}
