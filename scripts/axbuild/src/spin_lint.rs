use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use toml::Value;
use walkdir::{DirEntry, WalkDir};

const SPIN_VERSION_REQ: &str = "=0.12.0";
const SPIN_LOCKFILE_VERSION: &str = "0.12.0";
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const ALLOWED_SPIN_FEATURES: &[&str] = &["lock_api", "once", "lazylock"];
const FORBIDDEN_SPIN_RWLOCK_PATTERNS: &[&str] =
    &["spin::RwLock", "spin::rwlock", "use spin::RwLock"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Finding {
    path: PathBuf,
    location: String,
    message: String,
    help: String,
}

impl Finding {
    fn new(
        path: impl Into<PathBuf>,
        location: impl Into<String>,
        message: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            location: location.into(),
            message: message.into(),
            help: help.into(),
        }
    }
}

pub(crate) fn run_spin_lint_command() -> anyhow::Result<()> {
    let workspace_root = crate::context::workspace_root_path()?;
    let findings = lint_workspace(&workspace_root)?;

    if findings.is_empty() {
        println!("all spin-lint checks passed");
        return Ok(());
    }

    println!(
        "spin-lint found {} issue(s) across {} file(s):",
        findings.len(),
        findings
            .iter()
            .map(|finding| finding.path.clone())
            .collect::<HashSet<PathBuf>>()
            .len()
    );
    for finding in &findings {
        println!(
            "{}: {}: {}",
            finding.path.display(),
            finding.location,
            finding.message
        );
        println!("  help: {}", finding.help);
    }

    bail!("spin-lint found {} issue(s)", findings.len())
}

fn lint_workspace(workspace_root: &Path) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    check_no_local_spin_packages(workspace_root, &mut findings)?;
    check_root_manifest(workspace_root, &mut findings)?;
    check_workspace_manifests(workspace_root, &mut findings)?;
    check_no_spin_rwlock_usage(workspace_root, &mut findings)?;
    check_lockfile(workspace_root, &mut findings)?;
    Ok(findings)
}

fn check_no_local_spin_packages(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_manifest_entry)
    {
        let entry = entry.context("failed to walk workspace files")?;
        if !entry.file_type().is_file() || entry.file_name() != "Cargo.toml" {
            continue;
        }
        let manifest_path = entry.path();
        let manifest = read_toml(manifest_path)?;
        let package_name = manifest
            .get("package")
            .and_then(Value::as_table)
            .and_then(|table| table.get("name"))
            .and_then(Value::as_str);
        if package_name != Some("spin") {
            continue;
        }

        findings.push(Finding::new(
            manifest_path,
            "package.name",
            "local package named `spin` is not allowed",
            "remove the vendored package and depend on crates.io `spin` through the workspace",
        ));
    }

    Ok(())
}

fn should_visit_manifest_entry(entry: &DirEntry) -> bool {
    !entry.file_type().is_dir() || !is_ignored_dir_name(entry)
}

fn check_root_manifest(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let manifest = read_toml(&manifest_path)?;

    check_workspace_spin_dependency(&manifest_path, &manifest, findings);
    check_no_spin_patches(&manifest_path, &manifest, findings);

    Ok(())
}

fn check_workspace_spin_dependency(
    manifest_path: &Path,
    manifest: &Value,
    findings: &mut Vec<Finding>,
) {
    let dependency = manifest
        .get("workspace")
        .and_then(Value::as_table)
        .and_then(|table| table.get("dependencies"))
        .and_then(Value::as_table)
        .and_then(|table| table.get("spin"));

    let Some(dependency) = dependency else {
        findings.push(Finding::new(
            manifest_path,
            "workspace.dependencies.spin",
            "missing workspace spin dependency",
            format!(
                "add `spin = {{ version = \"{SPIN_VERSION_REQ}\", default-features = false, \
                 features = [\"lock_api\", \"once\", \"lazylock\"] }}`"
            ),
        ));
        return;
    };

    match dependency {
        Value::Table(table) => {
            check_no_source_override(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                findings,
            );
            check_no_package_rename(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                findings,
            );
            check_exact_dependency_version(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                findings,
            );
            check_default_features_disabled(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                findings,
            );
            check_allowed_features(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                true,
                findings,
            );
        }
        _ => findings.push(Finding::new(
            manifest_path,
            "workspace.dependencies.spin",
            "workspace spin dependency must be a table",
            format!(
                "use `spin = {{ version = \"{SPIN_VERSION_REQ}\", default-features = false, \
                 features = [\"lock_api\", \"once\", \"lazylock\"] }}`"
            ),
        )),
    }
}

fn check_no_spin_patches(manifest_path: &Path, manifest: &Value, findings: &mut Vec<Finding>) {
    let Some(patch_table) = manifest
        .get("patch")
        .and_then(Value::as_table)
        .and_then(|table| table.get("crates-io"))
        .and_then(Value::as_table)
    else {
        return;
    };

    for (key, value) in patch_table {
        if patch_package_name(key, value) != Some("spin") {
            continue;
        }
        findings.push(Finding::new(
            manifest_path,
            format!("patch.crates-io.{key}"),
            "crates.io patch for package `spin` is not allowed",
            "use the crates.io `spin` package through the root workspace dependency",
        ));
    }
}

fn check_workspace_manifests(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_manifest_entry)
    {
        let entry = entry.context("failed to walk workspace files")?;
        if !entry.file_type().is_file() || entry.file_name() != "Cargo.toml" {
            continue;
        }
        let manifest_path = entry.path();
        if manifest_path == workspace_root.join("Cargo.toml") {
            continue;
        }

        let manifest = read_toml(manifest_path)?;
        check_manifest_dependency_tables(manifest_path, &manifest, findings);
    }
    Ok(())
}

fn is_ignored_dir_name(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "tmp" | ".cache")
    )
}

fn check_manifest_dependency_tables(
    manifest_path: &Path,
    value: &Value,
    findings: &mut Vec<Finding>,
) {
    let Some(table) = value.as_table() else {
        return;
    };

    for (key, value) in table {
        if matches!(
            key.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) && let Some(dependencies) = value.as_table()
        {
            check_spin_dependency_table(manifest_path, key, dependencies, findings);
        }

        if value.is_table() {
            check_manifest_dependency_tables(manifest_path, value, findings);
        }
    }
}

fn check_spin_dependency_table(
    manifest_path: &Path,
    table_name: &str,
    dependencies: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    for (dependency_name, dependency) in dependencies {
        if !is_spin_dependency(dependency_name, dependency) {
            continue;
        }
        let location = format!("{table_name}.{dependency_name}");

        match dependency {
            Value::String(version_req) => {
                findings.push(Finding::new(
                    manifest_path,
                    location,
                    format!("spin version requirement `{version_req}` enables default features"),
                    "use `spin = { workspace = true }` or an explicit table with \
                     `default-features = false`",
                ));
            }
            Value::Table(table) => {
                if dependency_name != "spin" {
                    findings.push(Finding::new(
                        manifest_path,
                        &location,
                        "renamed spin dependency is not allowed",
                        "use `spin = { workspace = true }` so all crates share the same safe \
                         feature set",
                    ));
                }
                check_no_package_rename(manifest_path, &location, table, findings);
                check_no_source_override(manifest_path, &location, table, findings);
                if table.get("workspace").and_then(Value::as_bool) == Some(true) {
                    check_workspace_dependency_has_no_overrides(
                        manifest_path,
                        &location,
                        table,
                        findings,
                    );
                    continue;
                }
                check_exact_dependency_version(manifest_path, &location, table, findings);
                check_default_features_disabled(manifest_path, &location, table, findings);
                check_allowed_features(manifest_path, &location, table, false, findings);
            }
            _ => findings.push(Finding::new(
                manifest_path,
                location,
                "spin dependency must be a string or table",
                "use `spin = { workspace = true }`",
            )),
        }
    }
}

fn is_spin_dependency(key: &str, value: &Value) -> bool {
    key == "spin"
        || value
            .as_table()
            .and_then(|table| table.get("package"))
            .and_then(Value::as_str)
            == Some("spin")
}

fn check_no_spin_rwlock_usage(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_source_entry)
    {
        let entry = entry.context("failed to walk workspace source files")?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("rs")
        {
            continue;
        }

        let path = entry.path();
        if path == workspace_root.join("scripts/axbuild/src/spin_lint.rs") {
            continue;
        }
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        for (line_index, line) in contents.lines().enumerate() {
            for pattern in FORBIDDEN_SPIN_RWLOCK_PATTERNS {
                if line.contains(pattern) {
                    findings.push(Finding::new(
                        path,
                        format!("line {}", line_index + 1),
                        format!("forbidden `{pattern}` usage"),
                        "use `ax_kspin::SpinRwLock` for non-sleeping read-write locks",
                    ));
                }
            }
        }
    }

    Ok(())
}

fn should_visit_source_entry(entry: &DirEntry) -> bool {
    !entry.file_type().is_dir() || !is_ignored_source_dir_name(entry)
}

fn is_ignored_source_dir_name(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "tmp" | ".cache" | "docs")
    )
}

fn check_no_source_override(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    for key in ["path", "git", "registry"] {
        if table.contains_key(key) {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.{key}"),
                format!("spin dependency must not specify `{key}`"),
                "depend on crates.io `spin` through the root workspace dependency",
            ));
        }
    }
}

fn check_no_package_rename(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    if table.contains_key("package") {
        findings.push(Finding::new(
            manifest_path,
            format!("{location}.package"),
            "spin dependency must not rename package `spin`",
            "use the dependency key `spin` directly",
        ));
    }
}

fn check_exact_dependency_version(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    let version_req = table.get("version").and_then(Value::as_str);
    if version_req != Some(SPIN_VERSION_REQ) {
        findings.push(Finding::new(
            manifest_path,
            format!("{location}.version"),
            format!(
                "spin dependency version must stay at {SPIN_VERSION_REQ}, found `{}`",
                version_req.unwrap_or("<missing>")
            ),
            format!("pin spin with `version = \"{SPIN_VERSION_REQ}\"`"),
        ));
    }
}

fn check_default_features_disabled(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    match table.get("default-features") {
        Some(Value::Boolean(false)) => {}
        Some(Value::Boolean(true)) | None => findings.push(Finding::new(
            manifest_path,
            format!("{location}.default-features"),
            "spin default features must be disabled",
            "set `default-features = false` so upstream mutex/rwlock features stay unavailable",
        )),
        Some(_) => findings.push(Finding::new(
            manifest_path,
            format!("{location}.default-features"),
            "spin default-features must be a boolean",
            "set `default-features = false`",
        )),
    }
}

fn check_workspace_dependency_has_no_overrides(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    for key in ["features", "default-features", "version"] {
        if table.contains_key(key) {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.{key}"),
                "workspace spin dependency must not override workspace spin features",
                "use exactly `spin = { workspace = true }`",
            ));
        }
    }
}

fn check_allowed_features(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    require_all: bool,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    let Some(features) = table.get("features") else {
        if require_all {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.features"),
                "workspace spin dependency must list allowed features",
                "enable exactly `lock_api`, `once`, and `lazylock`",
            ));
        }
        return;
    };

    let Some(feature_values) = features.as_array() else {
        findings.push(Finding::new(
            manifest_path,
            format!("{location}.features"),
            "spin features must be an array",
            "use `features = [\"lock_api\", \"once\", \"lazylock\"]`",
        ));
        return;
    };

    let mut seen = HashSet::new();
    for (index, feature) in feature_values.iter().enumerate() {
        let Some(feature) = feature.as_str() else {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.features[{index}]"),
                "spin feature names must be strings",
                "remove the non-string feature entry",
            ));
            continue;
        };
        if !ALLOWED_SPIN_FEATURES.contains(&feature) {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.features[{index}]"),
                format!("spin feature `{feature}` is not allowed"),
                "only `lock_api`, `once`, and `lazylock` may be enabled",
            ));
        }
        seen.insert(feature);
    }

    if require_all {
        for required in ALLOWED_SPIN_FEATURES {
            if !seen.contains(required) {
                findings.push(Finding::new(
                    manifest_path,
                    format!("{location}.features"),
                    format!("workspace spin dependency must enable feature `{required}`"),
                    "enable exactly `lock_api`, `once`, and `lazylock`",
                ));
            }
        }
    }
}

fn patch_package_name<'a>(key: &'a str, value: &'a Value) -> Option<&'a str> {
    value
        .as_table()
        .and_then(|table| table.get("package"))
        .and_then(Value::as_str)
        .or(Some(key))
}

fn check_lockfile(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    let lock_path = workspace_root.join("Cargo.lock");
    let lockfile = read_toml(&lock_path)?;
    let packages = lockfile
        .get("package")
        .and_then(Value::as_array)
        .context("Cargo.lock is missing package entries")?;

    for package in packages {
        let Some(table) = package.as_table() else {
            continue;
        };
        if table.get("name").and_then(Value::as_str) != Some("spin") {
            continue;
        }

        let version = table
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        let location = format!("package spin {version}");
        let source = table.get("source").and_then(Value::as_str);
        let has_checksum = table.contains_key("checksum");

        if version != SPIN_LOCKFILE_VERSION {
            findings.push(Finding::new(
                &lock_path,
                &location,
                format!(
                    "spin lockfile version must stay at {SPIN_LOCKFILE_VERSION}, found `{version}`"
                ),
                format!("use crates.io `spin` {SPIN_LOCKFILE_VERSION}"),
            ));
        }
        if source != Some(CRATES_IO_SOURCE) {
            findings.push(Finding::new(
                &lock_path,
                &location,
                "spin lockfile entry must use crates.io registry source",
                format!("regenerate Cargo.lock so source is `{CRATES_IO_SOURCE}`"),
            ));
        }
        if !has_checksum {
            findings.push(Finding::new(
                &lock_path,
                &location,
                "spin lockfile entry must have a registry checksum",
                "regenerate Cargo.lock from crates.io",
            ));
        }
    }

    Ok(())
}

fn read_toml(path: &Path) -> anyhow::Result<Value> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn write_minimal_workspace(root: &Path) {
        write_file(
            root,
            "Cargo.toml",
            r#"
[workspace]
members = ["crate"]

[workspace.dependencies]
spin = { version = "=0.12.0", default-features = false, features = ["lock_api", "once", "lazylock"] }
"#,
        );
        write_file(
            root,
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin = { workspace = true }
"#,
        );
        write_file(
            root,
            "Cargo.lock",
            r#"
[[package]]
name = "spin"
version = "0.12.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
"#,
        );
    }

    #[test]
    fn accepts_workspace_registry_spin_dependency_without_crates_io_patch() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());

        let findings = lint_workspace(root.path()).unwrap();

        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn rejects_crates_io_spin_patch() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.toml",
            r#"
[workspace]
members = ["crate"]

[workspace.dependencies]
spin = { version = "=0.12.0", default-features = false, features = ["lock_api", "once", "lazylock"] }

[patch.crates-io]
spin = { path = "components/spin" }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("crates.io patch"))
        );
    }

    #[test]
    fn rejects_vendored_spin_copy() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "components/spin/Cargo.toml",
            r#"
[package]
name = "spin"
version = "0.12.0"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("local package named `spin`"))
        );
    }

    #[test]
    fn rejects_root_path_workspace_spin_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.toml",
            r#"
[workspace]
members = ["crate"]

[workspace.dependencies]
spin = { version = "=0.12.0", path = "components/spin", default-features = false, features = ["lock_api", "once", "lazylock"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.location == "workspace.dependencies.spin.path")
        );
    }

    #[test]
    fn rejects_root_spin_default_features_enabled() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.toml",
            r#"
[workspace]
members = ["crate"]

[workspace.dependencies]
spin = { version = "=0.12.0", default-features = true, features = ["lock_api", "once", "lazylock"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("default features must be disabled")
        }));
    }

    #[test]
    fn rejects_root_spin_rwlock_feature() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.toml",
            r#"
[workspace]
members = ["crate"]

[workspace.dependencies]
spin = { version = "=0.12.0", default-features = false, features = ["lock_api", "once", "lazylock", "rwlock"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("feature `rwlock` is not allowed"))
        );
    }

    #[test]
    fn accepts_explicit_registry_spin_dependency_with_allowed_features() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin = { version = "=0.12.0", default-features = false, features = ["once"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(findings.is_empty(), "{findings:#?}");
    }

    #[test]
    fn rejects_manifest_version_only_spin_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin = "0.12"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("default features"))
        );
    }

    #[test]
    fn rejects_explicit_spin_dependency_without_default_features_false() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin = { version = "=0.12.0", features = ["once"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("default features must be disabled")
        }));
    }

    #[test]
    fn rejects_explicit_spin_dependency_with_rwlock_feature() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin = { version = "=0.12.0", default-features = false, features = ["once", "rwlock"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("feature `rwlock` is not allowed"))
        );
    }

    #[test]
    fn rejects_renamed_spin_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin_compat = { package = "spin", version = "=0.12.0", default-features = false, features = ["once"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.location == "dependencies.spin_compat")
        );
    }

    #[test]
    fn rejects_workspace_spin_dependency_feature_override() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2021"

[dependencies]
spin = { workspace = true, features = ["rwlock"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("must not override workspace spin features")
        }));
    }

    #[test]
    fn rejects_lockfile_without_registry_source() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.lock",
            r#"
[[package]]
name = "spin"
version = "0.12.0"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("registry source"))
        );
    }

    #[test]
    fn rejects_lockfile_without_checksum() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.lock",
            r#"
[[package]]
name = "spin"
version = "0.12.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("registry checksum"))
        );
    }

    #[test]
    fn rejects_external_spin_future_version() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.lock",
            r#"
[[package]]
name = "spin"
version = "0.13.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("must stay at 0.12.0"))
        );
    }

    #[test]
    fn rejects_spin_rwlock_source_usage() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/src/lib.rs",
            r#"
pub fn bad() {
    let _lock = spin::RwLock::new(());
}
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("forbidden `spin::RwLock`"))
        );
    }
}
