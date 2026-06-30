use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use toml::Value;
use walkdir::{DirEntry, WalkDir};

const MAIN_SPIN_PATH: &str = "components/spin";
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
    check_vendored_spin_crates(workspace_root, &mut findings)?;
    check_root_manifest(workspace_root, &mut findings)?;
    check_workspace_manifests(workspace_root, &mut findings)?;
    check_no_spin_rwlock_usage(workspace_root, &mut findings)?;
    check_lockfile(workspace_root, &mut findings)?;
    Ok(findings)
}

fn check_vendored_spin_crates(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    check_vendored_spin_crate(
        workspace_root,
        MAIN_SPIN_PATH,
        "0.12.0",
        "main migration copy",
        findings,
    )?;
    check_forbidden_spin_crate_paths(workspace_root, findings)
}

fn check_vendored_spin_crate(
    workspace_root: &Path,
    relative_path: &str,
    expected_version: &str,
    label: &str,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    let manifest_path = workspace_root.join(relative_path).join("Cargo.toml");
    let Some(manifest) = read_toml_if_present(&manifest_path)? else {
        findings.push(Finding::new(
            &manifest_path,
            label,
            "vendored spin manifest is missing",
            format!("restore `{relative_path}` or update spin-lint if the migration copy changed"),
        ));
        return Ok(());
    };

    let package = manifest.get("package").and_then(Value::as_table);
    let name = package
        .and_then(|table| table.get("name"))
        .and_then(Value::as_str);
    let version = package
        .and_then(|table| table.get("version"))
        .and_then(Value::as_str);

    if name != Some("spin") || version != Some(expected_version) {
        findings.push(Finding::new(
            &manifest_path,
            label,
            format!(
                "expected package `spin` version `{expected_version}`, found name `{:?}` version \
                 `{:?}`",
                name, version
            ),
            format!(
                "keep `{relative_path}` as the registered vendored spin {expected_version} copy"
            ),
        ));
    }

    let features = manifest.get("features").and_then(Value::as_table);
    if let Some(features) = features {
        if features.contains_key("rwlock") {
            findings.push(Finding::new(
                &manifest_path,
                "features.rwlock",
                "vendored spin must not expose the upstream rwlock feature",
                "use `ax_kspin::SpinRwLock` instead of restoring `spin::RwLock`",
            ));
        }
        if let Some(default_features) = features.get("default").and_then(Value::as_array) {
            for (index, feature) in default_features.iter().enumerate() {
                if feature.as_str() == Some("rwlock") {
                    findings.push(Finding::new(
                        &manifest_path,
                        format!("features.default[{index}]"),
                        "vendored spin default features must not include rwlock",
                        "remove `rwlock` from the default feature set",
                    ));
                }
            }
        }
    }

    Ok(())
}

fn check_forbidden_spin_crate_paths(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    let registered_manifest =
        normalize_path(&workspace_root.join(MAIN_SPIN_PATH).join("Cargo.toml"));

    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_spin_crate_entry)
    {
        let entry = entry.context("failed to walk workspace files")?;
        if !entry.file_type().is_file() || entry.file_name() != "Cargo.toml" {
            continue;
        }
        let manifest_path = entry.path();
        if normalize_path(manifest_path) == registered_manifest {
            continue;
        }

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
            "legacy spin migration copy",
            "unregistered vendored spin copy is not allowed",
            format!(
                "remove this package; only `{MAIN_SPIN_PATH}` may remain until migration completes"
            ),
        ));
    }

    Ok(())
}

fn should_visit_spin_crate_entry(entry: &DirEntry) -> bool {
    !entry.file_type().is_dir() || !is_ignored_dir_name(entry)
}

fn check_root_manifest(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    let manifest_path = workspace_root.join("Cargo.toml");
    let manifest = read_toml(&manifest_path)?;

    check_workspace_spin_dependency(workspace_root, &manifest_path, &manifest, findings);
    check_no_spin_patches(&manifest_path, &manifest, findings);

    Ok(())
}

fn check_workspace_spin_dependency(
    workspace_root: &Path,
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
            format!("add `spin = {{ version = \"0.12\", path = \"{MAIN_SPIN_PATH}\" }}`"),
        ));
        return;
    };

    match dependency {
        Value::Table(table) => {
            check_no_external_source(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                findings,
            );
            check_dependency_version(
                manifest_path,
                "workspace.dependencies.spin",
                table,
                findings,
            );
            check_dependency_path(
                workspace_root,
                workspace_root,
                manifest_path,
                "workspace.dependencies.spin",
                table,
                &[MAIN_SPIN_PATH],
                findings,
            );
        }
        _ => findings.push(Finding::new(
            manifest_path,
            "workspace.dependencies.spin",
            "workspace spin dependency must be a table with a local path",
            format!("use `spin = {{ version = \"0.12\", path = \"{MAIN_SPIN_PATH}\" }}`"),
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
            "use the workspace dependency or an explicit path dependency for project crates",
        ));
    }
}

fn check_workspace_manifests(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(|entry| should_visit_entry(workspace_root, entry))
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
        check_manifest_dependency_tables(
            workspace_root,
            manifest_path,
            manifest_path.parent().unwrap_or(workspace_root),
            &manifest,
            findings,
        );
    }
    Ok(())
}

fn should_visit_entry(workspace_root: &Path, entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    let path = entry.path();
    path == workspace_root
        || !is_ignored_dir_name(entry) && !path_is_under(workspace_root, path, MAIN_SPIN_PATH)
}

fn is_ignored_dir_name(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "tmp" | ".cache")
    )
}

fn check_manifest_dependency_tables(
    workspace_root: &Path,
    manifest_path: &Path,
    manifest_dir: &Path,
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
            check_spin_dependency_table(
                workspace_root,
                manifest_path,
                manifest_dir,
                key,
                dependencies,
                findings,
            );
        }

        if value.is_table() {
            check_manifest_dependency_tables(
                workspace_root,
                manifest_path,
                manifest_dir,
                value,
                findings,
            );
        }
    }
}

fn check_spin_dependency_table(
    workspace_root: &Path,
    manifest_path: &Path,
    manifest_dir: &Path,
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
                    format!("spin version requirement `{version_req}` resolves through crates.io"),
                    "use `spin = { workspace = true }` or an explicit path to a registered local \
                     copy",
                ));
            }
            Value::Table(table) => {
                check_no_external_source(manifest_path, &location, table, findings);
                if table.get("workspace").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if table.contains_key("path") {
                    check_dependency_version(manifest_path, &location, table, findings);
                    check_dependency_path(
                        manifest_dir,
                        workspace_root,
                        manifest_path,
                        &location,
                        table,
                        &[MAIN_SPIN_PATH],
                        findings,
                    );
                    continue;
                }
                findings.push(Finding::new(
                    manifest_path,
                    location,
                    "spin dependency must not rely on crates.io version resolution",
                    "use `spin = { workspace = true }` or an explicit path to a registered local \
                     copy",
                ));
            }
            _ => findings.push(Finding::new(
                manifest_path,
                location,
                "spin dependency must be a string or table",
                "use `spin = { workspace = true }` or the registered local path",
            )),
        }
    }

    let _ = workspace_root;
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
        .filter_entry(|entry| should_visit_source_entry(workspace_root, entry))
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

fn should_visit_source_entry(workspace_root: &Path, entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    let path = entry.path();
    path == workspace_root
        || !is_ignored_source_dir_name(entry)
            && !path_is_under(workspace_root, path, MAIN_SPIN_PATH)
}

fn is_ignored_source_dir_name(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "tmp" | ".cache" | "docs")
    )
}

fn check_no_external_source(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    for key in ["registry", "git"] {
        if table.contains_key(key) {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.{key}"),
                format!("spin dependency must not specify `{key}`"),
                "route spin through the root workspace dependency or an explicit local path",
            ));
        }
    }
}

fn check_dependency_version(
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    let Some(version_req) = table.get("version").and_then(Value::as_str) else {
        return;
    };
    if !is_allowed_spin_version_req(version_req) {
        findings.push(Finding::new(
            manifest_path,
            format!("{location}.version"),
            format!("spin version requirement `{version_req}` is not registered"),
            "use the workspace dependency or the vendored migration version",
        ));
    }
}

fn check_dependency_path(
    actual_base_dir: &Path,
    allowed_base_dir: &Path,
    manifest_path: &Path,
    location: impl Into<String>,
    table: &toml::Table,
    allowed_relative_paths: &[&str],
    findings: &mut Vec<Finding>,
) {
    let location = location.into();
    let Some(path) = table.get("path").and_then(Value::as_str) else {
        findings.push(Finding::new(
            manifest_path,
            format!("{location}.path"),
            "spin dependency must point at a registered local migration copy",
            format!(
                "use one of: {}",
                allowed_relative_paths
                    .iter()
                    .map(|path| format!("`{path}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
        return;
    };

    let actual = normalize_path(&actual_base_dir.join(path));
    let allowed = allowed_relative_paths
        .iter()
        .map(|allowed| normalize_path(&allowed_base_dir.join(allowed)))
        .collect::<Vec<_>>();

    if !allowed.contains(&actual) {
        findings.push(Finding::new(
            manifest_path,
            format!("{location}.path"),
            format!("spin dependency path `{path}` is not registered"),
            format!(
                "use one of: {}",
                allowed_relative_paths
                    .iter()
                    .map(|path| format!("`{path}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
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

        if let Some(source) = source {
            findings.push(Finding::new(
                &lock_path,
                &location,
                format!(
                    "external spin package appears in the resolved dependency graph from \
                     `{source}`"
                ),
                "remove the dependency chain or disable the upstream feature that pulls crates.io \
                 `spin`; do not add a crates.io patch",
            ));
        }
        if has_checksum {
            findings.push(Finding::new(
                &lock_path,
                &location,
                "external spin package has a registry checksum",
                "local path spin packages must not carry a crates.io checksum in Cargo.lock",
            ));
        }
        if source.is_none() && !has_checksum && version != "0.12.0" {
            findings.push(Finding::new(
                &lock_path,
                &location,
                format!("local spin version `{version}` is not registered for the migration"),
                format!("use the vendored `{MAIN_SPIN_PATH}` package or remove the dependency"),
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

fn read_toml_if_present(path: &Path) -> anyhow::Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            Ok(Some(toml::from_str(&contents).with_context(|| {
                format!("failed to parse {}", path.display())
            })?))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn is_allowed_spin_version_req(version_req: &str) -> bool {
    matches!(
        version_req.trim(),
        "0.12" | "0.12.0" | "^0.12" | "^0.12.0" | "=0.12.0"
    )
}

fn path_is_under(workspace_root: &Path, path: &Path, relative_parent: &str) -> bool {
    normalize_path(path).starts_with(normalize_path(&workspace_root.join(relative_parent)))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
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
spin = { version = "0.12", path = "components/spin" }
"#,
        );
        write_file(
            root,
            "components/spin/Cargo.toml",
            r#"
[package]
name = "spin"
version = "0.12.0"
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
"#,
        );
    }

    #[test]
    fn accepts_workspace_spin_dependency_without_crates_io_patch() {
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
spin = { version = "0.12", path = "components/spin" }

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
    fn rejects_explicit_external_manifest_source() {
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
spin = { version = "0.12", registry = "crates-io" }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("must not specify `registry`"))
        );
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
spin = { version = "0.12", default-features = false, features = ["once"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("crates.io version resolution"))
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
spin_compat = { package = "spin", version = "0.12" }
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
    fn rejects_transitive_external_spin_lockfile_entry() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.lock",
            r#"
[[package]]
name = "dependency"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
dependencies = [
 "spin 0.10.0",
]

[[package]]
name = "spin"
version = "0.10.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "def"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("resolved dependency graph"))
        );
    }

    #[test]
    fn rejects_external_spin_current_migration_version() {
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
checksum = "abc"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("resolved dependency graph"))
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
                .any(|finding| finding.message.contains("resolved dependency graph"))
        );
    }

    #[test]
    fn rejects_vendored_spin_rwlock_feature() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "components/spin/Cargo.toml",
            r#"
[package]
name = "spin"
version = "0.12.0"

[features]
default = ["rwlock", "once"]
rwlock = []
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("must not expose"))
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("must not include rwlock"))
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

    #[test]
    fn rejects_unregistered_local_spin_version() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "Cargo.lock",
            r#"
[[package]]
name = "spin"
version = "0.10.0"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("not registered"))
        );
    }

    #[test]
    fn rejects_legacy_vendored_spin_copy() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "components/spin-0.10/Cargo.toml",
            r#"
[package]
name = "spin"
version = "0.10.0"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("unregistered vendored spin copy"))
        );
    }

    #[test]
    fn accepts_registered_manifest_path_relative_to_crate() {
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
spin = { version = "0.12", path = "../components/spin" }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();

        assert!(findings.is_empty(), "{findings:#?}");
    }
}
