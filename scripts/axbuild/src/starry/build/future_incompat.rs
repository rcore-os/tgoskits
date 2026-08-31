use std::{collections::BTreeMap, fs, path::Path, sync::OnceLock};

use anyhow::{Context, bail};
use regex::Regex;
use serde::Deserialize;

const FUTURE_INCOMPAT_REPORT: &str = ".future-incompat-report.json";
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

pub(super) fn check_aarch64_future_incompat_report(
    cargo_artifact_dir: &Path,
) -> anyhow::Result<()> {
    let report_path = cargo_target_dir(cargo_artifact_dir)?.join(FUTURE_INCOMPAT_REPORT);
    let packages = validate_report_path(&report_path)?;
    if !packages.is_empty() {
        println!(
            "[axbuild] accepted known upstream Rust #134375 future-incompatibility for {}",
            packages.join(", ")
        );
    }
    Ok(())
}

fn cargo_target_dir(cargo_artifact_dir: &Path) -> anyhow::Result<&Path> {
    cargo_artifact_dir
        .parent()
        .and_then(Path::parent)
        .with_context(|| {
            format!(
                "cannot locate Cargo target directory from artifact directory {}",
                cargo_artifact_dir.display()
            )
        })
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
    let report: FutureIncompatReportFile =
        serde_json::from_str(content).context("invalid Cargo future-incompatibility JSON")?;
    if report.version != REPORT_VERSION {
        bail!(
            "unsupported Cargo future-incompatibility report version {}",
            report.version
        );
    }

    let mut report_ids = report
        .reports
        .iter()
        .map(|report| report.id)
        .collect::<Vec<_>>();
    report_ids.sort_unstable();
    let expected_ids = (1..=u64::try_from(report_ids.len())?).collect::<Vec<_>>();
    if report_ids != expected_ids {
        bail!(
            "invalid Cargo future-incompatibility report ids: expected {expected_ids:?}, found \
             {report_ids:?}"
        );
    }
    let expected_next_id = u64::try_from(report_ids.len())?
        .checked_add(1)
        .context("Cargo future-incompatibility report id overflow")?;
    if report.next_id != expected_next_id {
        bail!(
            "invalid Cargo future-incompatibility report ids: expected next_id \
             {expected_next_id}, found {}",
            report.next_id
        );
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
}
