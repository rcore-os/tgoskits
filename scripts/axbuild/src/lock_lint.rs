use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use toml::Value;
use walkdir::{DirEntry, WalkDir};

const REMOVED_LOCK_PACKAGES: &[&str] = &["ax-kspin", "ax-kernel-guard", "ax-lockdep"];
const REMOVED_LOCK_IMPORTS: &[&str] = &["ax_kspin", "ax_kernel_guard", "ax_lockdep"];
const DIRECT_SPIN_PATTERNS: &[&str] = &["use spin", "extern crate spin"];
const PROVIDER_TRAITS: &[&str] = &["CriticalSectionOps", "MutexRuntimeOps", "LockdepOps"];
const RUNTIME_PROVIDER_PATH: &str = "os/arceos/modules/axruntime/src/sync.rs";
const HOST_PROVIDER_PATHS: &[&str] = &[
    "os/arceos/modules/axsync/src/context.rs",
    "os/arceos/modules/axsync/src/mutex.rs",
];
const HOST_PROVIDER_CFG: &str =
    "#[cfg(all(feature = \"host-test\", not(target_os = \"none\")))]\nmod host {";

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

pub(crate) fn run_lock_lint_command() -> anyhow::Result<()> {
    let workspace_root = crate::context::workspace_root_path()?;
    let findings = lint_workspace(&workspace_root)?;

    if findings.is_empty() {
        println!("all lock-lint checks passed");
        return Ok(());
    }

    println!(
        "lock-lint found {} issue(s) across {} file(s):",
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

    bail!("lock-lint found {} issue(s)", findings.len())
}

fn lint_workspace(workspace_root: &Path) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    check_manifests(workspace_root, &mut findings)?;
    check_source_boundaries(workspace_root, &mut findings)?;
    check_runtime_providers(workspace_root, &mut findings)?;
    check_lockfile(workspace_root, &mut findings)?;
    Ok(findings)
}

fn check_manifests(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_entry)
    {
        let entry = entry.context("failed to walk workspace manifests")?;
        if !entry.file_type().is_file() || entry.file_name() != "Cargo.toml" {
            continue;
        }

        let path = entry.path();
        let manifest = read_toml(path)?;
        if let Some(package_name) = manifest
            .get("package")
            .and_then(Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(Value::as_str)
            && (package_name == "spin" || REMOVED_LOCK_PACKAGES.contains(&package_name))
        {
            findings.push(Finding::new(
                path,
                "package.name",
                format!("removed lock package `{package_name}` is not allowed"),
                "use the ax-sync public interfaces",
            ));
        }

        if path == workspace_root.join("Cargo.toml") {
            check_removed_workspace_members(path, &manifest, findings);
        }
        check_dependency_tables(path, &manifest, findings);
    }
    Ok(())
}

fn check_removed_workspace_members(
    manifest_path: &Path,
    manifest: &Value,
    findings: &mut Vec<Finding>,
) {
    let Some(members) = manifest
        .get("workspace")
        .and_then(Value::as_table)
        .and_then(|workspace| workspace.get("members"))
        .and_then(Value::as_array)
    else {
        return;
    };

    for (index, member) in members.iter().enumerate() {
        let Some(member) = member.as_str() else {
            continue;
        };
        if [
            "components/kspin",
            "components/kernel_guard",
            "components/lockdep",
        ]
        .iter()
        .any(|removed| member == *removed || member.starts_with(&format!("{removed}/")))
        {
            findings.push(Finding::new(
                manifest_path,
                format!("workspace.members[{index}]"),
                format!("removed lock crate path `{member}` is still a workspace member"),
                "remove the member; its functionality belongs to ax-sync",
            ));
        }
    }
}

fn check_dependency_tables(manifest_path: &Path, value: &Value, findings: &mut Vec<Finding>) {
    let Some(table) = value.as_table() else {
        return;
    };

    for (key, value) in table {
        if matches!(
            key.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) && let Some(dependencies) = value.as_table()
        {
            for (dependency_name, dependency) in dependencies {
                let package_name = dependency
                    .as_table()
                    .and_then(|dependency| dependency.get("package"))
                    .and_then(Value::as_str)
                    .unwrap_or(dependency_name);
                let location = format!("{key}.{dependency_name}");

                if package_name == "spin" {
                    findings.push(Finding::new(
                        manifest_path,
                        &location,
                        "first-party crates must not directly depend on crates.io `spin`",
                        "use ax-lazyinit for OnceLock/LazyLock or ax-sync for lock primitives",
                    ));
                }
                if REMOVED_LOCK_PACKAGES.contains(&package_name) {
                    findings.push(Finding::new(
                        manifest_path,
                        &location,
                        format!("dependency on removed lock crate `{package_name}`"),
                        "depend on ax-sync and select context policy at lock acquisition",
                    ));
                }
                if is_axvisor_manifest(manifest_path) && package_name == "ax-sync" {
                    findings.push(Finding::new(
                        manifest_path,
                        &location,
                        "Axvisor must not depend directly on ax-sync",
                        "use std::sync normally and ax_std::os::arceos::sync in special contexts",
                    ));
                }
            }
        }

        if value.is_table() {
            check_dependency_tables(manifest_path, value, findings);
        }
    }
}

fn check_source_boundaries(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_source_entry)
    {
        let entry = entry.context("failed to walk workspace source files")?;
        if !entry.file_type().is_file()
            || entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("rs")
        {
            continue;
        }

        let path = entry.path();
        if path == workspace_root.join("scripts/axbuild/src/lock_lint.rs") {
            continue;
        }
        let relative = relative_path(workspace_root, path);
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        for (line_index, line) in source_lines_without_comments(&contents).iter().enumerate() {
            if contains_direct_spin_path(line) {
                findings.push(Finding::new(
                    path,
                    format!("line {}", line_index + 1),
                    "direct crates.io spin API `spin::` is not allowed",
                    "use ax-lazyinit, ax-sync, or std::sync according to the component boundary",
                ));
            }
            for pattern in DIRECT_SPIN_PATTERNS {
                if line.contains(pattern) {
                    findings.push(Finding::new(
                        path,
                        format!("line {}", line_index + 1),
                        format!("direct crates.io spin API `{pattern}` is not allowed"),
                        "use ax-lazyinit, ax-sync, or std::sync according to the component \
                         boundary",
                    ));
                }
            }
            for import in REMOVED_LOCK_IMPORTS {
                if line.contains(import) {
                    findings.push(Finding::new(
                        path,
                        format!("line {}", line_index + 1),
                        format!("import from removed lock crate `{import}`"),
                        "use ax-sync directly",
                    ));
                }
            }

            if is_starry_kernel_source(&relative)
                && relative != "os/StarryOS/kernel/src/sync.rs"
                && (line.contains("ax_sync::") || line.contains("use ax_sync"))
            {
                findings.push(Finding::new(
                    path,
                    format!("line {}", line_index + 1),
                    "Starry kernel lock code bypasses crate::sync",
                    "import synchronization primitives from crate::sync",
                ));
            }

            if is_axvisor_source(&relative)
                && (line.contains("ax_sync::") || line.contains("use ax_sync"))
            {
                findings.push(Finding::new(
                    path,
                    format!("line {}", line_index + 1),
                    "Axvisor code bypasses its std/ax_std synchronization boundary",
                    "use std::sync normally or ax_std::os::arceos::sync for special contexts",
                ));
            }
        }
    }
    Ok(())
}

fn contains_direct_spin_path(line: &str) -> bool {
    line.match_indices("spin::").any(|(index, _)| {
        if index == 0 {
            return true;
        }
        let prefix = &line[..index];
        let previous = prefix.chars().next_back().unwrap();
        if previous != ':' {
            return !previous.is_alphanumeric() && previous != '_';
        }
        let qualifier = prefix.strip_suffix("::").unwrap_or(prefix);
        !qualifier
            .chars()
            .next_back()
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
    })
}

fn check_runtime_providers(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    let mut runtime_counts = [0usize; 3];

    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(should_visit_source_entry)
    {
        let entry = entry.context("failed to walk runtime provider sources")?;
        if !entry.file_type().is_file()
            || entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("rs")
        {
            continue;
        }

        let relative = relative_path(workspace_root, entry.path());
        if relative == "scripts/axbuild/src/lock_lint.rs" {
            continue;
        }
        let contents = fs::read_to_string(entry.path())
            .with_context(|| format!("failed to read {}", entry.path().display()))?;
        for (trait_index, trait_name) in PROVIDER_TRAITS.iter().enumerate() {
            let qualified = format!("impl ax_sync::{trait_name} for");
            let local = format!("impl {trait_name} for");
            let occurrences =
                contents.matches(&qualified).count() + contents.matches(&local).count();
            if occurrences == 0 {
                continue;
            }

            if relative == RUNTIME_PROVIDER_PATH {
                runtime_counts[trait_index] += occurrences;
            } else if !is_allowed_test_provider(&relative, trait_name) {
                findings.push(Finding::new(
                    entry.path(),
                    trait_name.to_string(),
                    format!("{trait_name} provider exists outside ax-runtime"),
                    "production builds must obtain exactly one provider from ax-runtime",
                ));
            }
        }
    }

    for (trait_name, count) in PROVIDER_TRAITS.iter().zip(runtime_counts) {
        if count != 1 {
            findings.push(Finding::new(
                workspace_root.join(RUNTIME_PROVIDER_PATH),
                trait_name.to_string(),
                format!("expected exactly one ax-runtime {trait_name} provider, found {count}"),
                "define the production capability provider exactly once in ax-runtime/src/sync.rs",
            ));
        }
    }
    check_provider_cfgs(workspace_root, findings)?;
    Ok(())
}

fn check_provider_cfgs(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    let runtime_path = workspace_root.join(RUNTIME_PROVIDER_PATH);
    if runtime_path.exists() {
        let contents = fs::read_to_string(&runtime_path)
            .with_context(|| format!("failed to read {}", runtime_path.display()))?;
        if contents.contains("target_os = \"none\"")
            || !contents.contains("not(feature = \"host-test\")")
        {
            findings.push(Finding::new(
                &runtime_path,
                "provider cfg",
                "ax-runtime providers are not selected by the explicit host-test boundary",
                "gate production providers with not(feature = \"host-test\"); custom std targets \
                 are still ArceOS production builds",
            ));
        }
    }

    for relative in HOST_PROVIDER_PATHS {
        let path = workspace_root.join(relative);
        if !path.exists() {
            continue;
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if !contents.contains(HOST_PROVIDER_CFG) {
            findings.push(Finding::new(
                &path,
                "host provider cfg",
                "ax-sync host provider is not restricted to host-test on std-capable targets",
                "gate the host provider with all(feature = \"host-test\", not(target_os = \
                 \"none\"))",
            ));
        }
    }
    Ok(())
}

fn is_allowed_test_provider(relative: &str, trait_name: &str) -> bool {
    (relative == "os/arceos/modules/axsync/src/context.rs" && trait_name == "CriticalSectionOps")
        || (relative == "os/arceos/modules/axsync/src/mutex.rs" && trait_name == "MutexRuntimeOps")
}

fn check_lockfile(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    let lock_path = workspace_root.join("Cargo.lock");
    if !lock_path.exists() {
        return Ok(());
    }
    let lockfile = read_toml(&lock_path)?;
    let Some(packages) = lockfile.get("package").and_then(Value::as_array) else {
        return Ok(());
    };

    for package in packages {
        let Some(name) = package
            .as_table()
            .and_then(|package| package.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if REMOVED_LOCK_PACKAGES.contains(&name) {
            findings.push(Finding::new(
                &lock_path,
                format!("package {name}"),
                format!("removed lock package `{name}` remains in Cargo.lock"),
                "regenerate Cargo.lock after removing the dependency",
            ));
        }
    }
    Ok(())
}

fn source_lines_without_comments(contents: &str) -> Vec<String> {
    let mut in_block_comment = false;
    contents
        .lines()
        .map(|line| {
            let mut remaining = line;
            let mut code = String::new();
            loop {
                if in_block_comment {
                    let Some(end) = remaining.find("*/") else {
                        break;
                    };
                    remaining = &remaining[end + 2..];
                    in_block_comment = false;
                    continue;
                }
                let line_comment = remaining.find("//").unwrap_or(remaining.len());
                let block_comment = remaining.find("/*").unwrap_or(remaining.len());
                let end = line_comment.min(block_comment);
                code.push_str(&remaining[..end]);
                if end == line_comment {
                    break;
                }
                in_block_comment = true;
                remaining = &remaining[block_comment + 2..];
            }
            code
        })
        .collect()
}

fn is_starry_kernel_source(relative: &str) -> bool {
    relative.starts_with("os/StarryOS/kernel/src/")
}

fn is_axvisor_manifest(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized.ends_with("/virtualization/axvm/Cargo.toml")
        || normalized.ends_with("/os/axvisor/Cargo.toml")
}

fn is_axvisor_source(relative: &str) -> bool {
    relative.starts_with("virtualization/axvm/src/") || relative.starts_with("os/axvisor/src/")
}

fn relative_path(workspace_root: &Path, path: &Path) -> String {
    path.strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn should_visit_entry(entry: &DirEntry) -> bool {
    !entry.file_type().is_dir() || !is_ignored_dir(entry)
}

fn should_visit_source_entry(entry: &DirEntry) -> bool {
    should_visit_entry(entry)
        && (!entry.file_type().is_dir() || entry.file_name().to_str() != Some("docs"))
}

fn is_ignored_dir(entry: &DirEntry) -> bool {
    matches!(
        entry.file_name().to_str(),
        Some(".git" | "target" | "tmp" | ".cache")
    )
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
"#,
        );
        write_file(
            root,
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2024"
"#,
        );
        write_file(root, "Cargo.lock", "version = 4\n");
        write_file(
            root,
            RUNTIME_PROVIDER_PATH,
            r#"
#[cfg(not(feature = "host-test"))]
impl ax_sync::CriticalSectionOps for RuntimeCriticalSectionOps {}
#[cfg(not(feature = "host-test"))]
impl ax_sync::MutexRuntimeOps for RuntimeMutexOps {}
#[cfg(not(feature = "host-test"))]
impl ax_sync::LockdepOps for RuntimeLockdepOps {}
"#,
        );
    }

    #[test]
    fn accepts_unified_lock_workspace() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());

        assert!(lint_workspace(root.path()).unwrap().is_empty());
    }

    #[test]
    fn rejects_direct_spin_dependency_and_source_use() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2024"
[dependencies]
spin = "0.12"
"#,
        );
        write_file(root.path(), "crate/src/lib.rs", "use spin::Once;\n");

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("directly depend"))
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("direct crates.io"))
        );
    }

    #[test]
    fn rejects_absolute_direct_spin_source_use() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(root.path(), "crate/src/lib.rs", "use ::spin::Once;\n");

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("direct crates.io"))
        );
    }

    #[test]
    fn rejects_removed_lock_crate_alias() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2024"
[dependencies]
legacy = { package = "ax-lockdep", version = "0.1" }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("ax-lockdep"))
        );
    }

    #[test]
    fn rejects_starry_kernel_facade_bypass() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "os/StarryOS/kernel/src/task.rs",
            "use ax_sync::SpinLock;\n",
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("crate::sync"))
        );
    }

    #[test]
    fn rejects_axvisor_low_level_dependency_and_import() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "virtualization/axvm/Cargo.toml",
            r#"
[package]
name = "axvm"
version = "0.1.0"
edition = "2024"
[dependencies]
ax-sync = "0.1"
"#,
        );
        write_file(
            root.path(),
            "virtualization/axvm/src/lib.rs",
            "use ax_sync::SpinLock;\n",
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("must not depend"))
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("Axvisor code"))
        );
    }

    #[test]
    fn rejects_second_production_provider() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/src/provider.rs",
            "impl ax_sync::CriticalSectionOps for OtherRuntime {}\n",
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("outside ax-runtime"))
        );
    }

    #[test]
    fn rejects_target_os_based_provider_selection() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            RUNTIME_PROVIDER_PATH,
            r#"
#[cfg(target_os = "none")]
impl ax_sync::CriticalSectionOps for RuntimeCriticalSectionOps {}
impl ax_sync::MutexRuntimeOps for RuntimeMutexOps {}
impl ax_sync::LockdepOps for RuntimeLockdepOps {}
"#,
        );
        write_file(
            root.path(),
            HOST_PROVIDER_PATHS[0],
            r#"
#[cfg(not(target_os = "none"))]
mod host {
    impl CriticalSectionOps for HostCriticalSectionOps {}
}
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| { finding.message.contains("explicit host-test boundary") })
        );
        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("not restricted to host-test on std-capable targets")
        }));
    }

    #[test]
    fn accepts_target_aware_host_provider_selection() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        for relative in HOST_PROVIDER_PATHS {
            write_file(
                root.path(),
                relative,
                r#"
#[cfg(all(feature = "host-test", not(target_os = "none")))]
mod host {}
"#,
            );
        }

        assert!(lint_workspace(root.path()).unwrap().is_empty());
    }
}
