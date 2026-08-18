use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use toml::Value;
use walkdir::{DirEntry, WalkDir};

const REMOVED_LOCK_PACKAGES: &[&str] = &[
    "ax-kspin",
    "ax-kernel-guard",
    "ax-lockdep",
    "ax-sync-test-support",
];
const REMOVED_LOCK_IMPORTS: &[&str] = &[
    "ax_kspin",
    "ax_kernel_guard",
    "ax_lockdep",
    "ax_sync_test_support",
];
const REMOVED_AX_SYNC_FEATURES: &[&str] = &["smp", "lockdep"];
const DIRECT_SPIN_PATTERNS: &[&str] = &["use spin", "extern crate spin"];
const PROVIDER_TRAITS: &[&str] = &[
    "ContextOps",
    "SpinOps",
    "RwLockOps",
    "MutexOps",
    "LockdepOps",
];
const RUNTIME_PROVIDER_PATH: &str = "os/arceos/modules/axruntime/src/sync.rs";
const AX_SYNC_HOST_MODULE_PATH: &str = "os/arceos/modules/axsync/src/lib.rs";
const AX_SYNC_OS_EDGE_ALLOWLIST: &[&str] = &[
    "os/arceos/modules/axruntime/Cargo.toml",
    "os/arceos/modules/axhal/Cargo.toml",
    "os/arceos/modules/axmm/Cargo.toml",
    "os/arceos/modules/axipi/Cargo.toml",
];

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
    let workspace_manifest = read_toml(&workspace_root.join("Cargo.toml"))?;
    let internal_workspace_dependencies =
        collect_internal_workspace_dependencies(&workspace_manifest);

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
        check_dependency_tables(
            workspace_root,
            path,
            &manifest,
            &internal_workspace_dependencies,
            findings,
        );
        check_removed_feature_forwarding(path, &manifest, "manifest", findings);
    }
    Ok(())
}

fn collect_internal_workspace_dependencies(manifest: &Value) -> HashSet<String> {
    manifest
        .get("workspace")
        .and_then(Value::as_table)
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Value::as_table)
        .into_iter()
        .flat_map(|dependencies| dependencies.iter())
        .filter_map(|(name, dependency)| {
            dependency
                .as_table()
                .and_then(|dependency| dependency.get("path"))
                .and_then(Value::as_str)
                .map(|_| name.clone())
        })
        .collect()
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

fn check_dependency_tables(
    workspace_root: &Path,
    manifest_path: &Path,
    value: &Value,
    internal_workspace_dependencies: &HashSet<String>,
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
            for (dependency_name, dependency) in dependencies {
                let package_name = dependency
                    .as_table()
                    .and_then(|dependency| dependency.get("package"))
                    .and_then(Value::as_str)
                    .unwrap_or(dependency_name);
                let location = format!("{key}.{dependency_name}");

                if key == "dev-dependencies" {
                    check_internal_dev_dependency(
                        workspace_root,
                        manifest_path,
                        dependency_name,
                        dependency,
                        internal_workspace_dependencies,
                        &location,
                        findings,
                    );
                }

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
                if package_name == "ax-sync" {
                    check_removed_dependency_features(
                        manifest_path,
                        &location,
                        dependency,
                        findings,
                    );
                }
                if package_name == "ax-sync"
                    && is_os_layer_manifest(workspace_root, manifest_path)
                    && !is_allowed_ax_sync_os_edge(workspace_root, manifest_path)
                {
                    findings.push(Finding::new(
                        manifest_path,
                        &location,
                        "OS-layer crate must not depend directly on ax-sync",
                        "use ax-runtime::sync, crate::sync, or ax_std::os::arceos::sync; only \
                         documented cycle-breaking edges may use ax-sync directly",
                    ));
                }
            }
        }

        if value.is_table() {
            check_dependency_tables(
                workspace_root,
                manifest_path,
                value,
                internal_workspace_dependencies,
                findings,
            );
        }
    }
}

fn check_internal_dev_dependency(
    workspace_root: &Path,
    manifest_path: &Path,
    dependency_name: &str,
    dependency: &Value,
    internal_workspace_dependencies: &HashSet<String>,
    location: &str,
    findings: &mut Vec<Finding>,
) {
    let Some(dependency) = dependency.as_table() else {
        return;
    };

    let inherits_workspace_version = dependency
        .get("workspace")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && internal_workspace_dependencies.contains(dependency_name);
    if inherits_workspace_version {
        findings.push(Finding::new(
            manifest_path,
            location,
            "workspace-internal dev-dependency must not inherit a version",
            "use a relative path-only dev-dependency so cargo publish strips it",
        ));
        return;
    }

    if dependency.contains_key("version")
        && dependency_path_is_inside_workspace(workspace_root, manifest_path, dependency)
    {
        findings.push(Finding::new(
            manifest_path,
            location,
            "workspace-internal dev-dependency must not specify a version",
            "remove `version` and keep only the relative path and required test features",
        ));
    }
}

fn dependency_path_is_inside_workspace(
    workspace_root: &Path,
    manifest_path: &Path,
    dependency: &toml::value::Table,
) -> bool {
    let Some(dependency_path) = dependency.get("path").and_then(Value::as_str) else {
        return false;
    };
    let Some(manifest_dir) = manifest_path.parent() else {
        return false;
    };
    let Ok(workspace_root) = workspace_root.canonicalize() else {
        return false;
    };
    let Ok(dependency_path) = manifest_dir.join(dependency_path).canonicalize() else {
        return false;
    };

    dependency_path.starts_with(workspace_root)
}

fn check_removed_dependency_features(
    manifest_path: &Path,
    location: &str,
    dependency: &Value,
    findings: &mut Vec<Finding>,
) {
    let Some(features) = dependency
        .as_table()
        .and_then(|dependency| dependency.get("features"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for feature in features.iter().filter_map(Value::as_str) {
        if REMOVED_AX_SYNC_FEATURES.contains(&feature) {
            findings.push(Finding::new(
                manifest_path,
                format!("{location}.features"),
                format!("removed ax-sync feature `{feature}` is still requested"),
                "SMP and lockdep behavior belong to the selected runtime engine",
            ));
        }
    }
}

fn check_removed_feature_forwarding(
    manifest_path: &Path,
    value: &Value,
    location: &str,
    findings: &mut Vec<Finding>,
) {
    match value {
        Value::String(feature) => {
            if REMOVED_AX_SYNC_FEATURES
                .iter()
                .any(|removed| feature == &format!("ax-sync/{removed}"))
            {
                findings.push(Finding::new(
                    manifest_path,
                    location,
                    format!("removed ax-sync feature forwarding `{feature}` remains"),
                    "remove the forwarding; the provider selects SMP and lockdep behavior",
                ));
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                check_removed_feature_forwarding(
                    manifest_path,
                    value,
                    &format!("{location}[{index}]"),
                    findings,
                );
            }
        }
        Value::Table(table) => {
            for (key, value) in table {
                check_removed_feature_forwarding(
                    manifest_path,
                    value,
                    &format!("{location}.{key}"),
                    findings,
                );
            }
        }
        _ => {}
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

        let mut internal_use_tree_depth = 0usize;
        for (line_index, line) in source_lines_without_comments(&contents).iter().enumerate() {
            let in_internal_use_tree =
                line_is_in_internal_use_tree(line, &mut internal_use_tree_depth);
            if !in_internal_use_tree && contains_direct_spin_path(line) {
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
                && (line.contains("ax_sync::") || line.contains("use ax_sync"))
            {
                findings.push(Finding::new(
                    path,
                    format!("line {}", line_index + 1),
                    "Starry kernel lock code bypasses crate::sync",
                    "import synchronization primitives from crate::sync",
                ));
            }

            if is_starry_kernel_source(&relative)
                && relative != "os/StarryOS/kernel/src/sync.rs"
                && line.contains("ax_runtime::sync")
            {
                findings.push(Finding::new(
                    path,
                    format!("line {}", line_index + 1),
                    "Starry kernel lock code bypasses crate::sync runtime facade",
                    "import synchronization primitives from crate::sync",
                ));
            }

            if relative.starts_with("os/arceos/modules/axtask/src/")
                && (line.contains("ax_sync::") || line.contains("use ax_sync"))
            {
                findings.push(Finding::new(
                    path,
                    format!("line {}", line_index + 1),
                    "ax-task must not depend on the ax-sync bridge",
                    "use the native crate::sync implementation",
                ));
            }

            if is_axvisor_source(&relative)
                && (line.contains("ax_sync::")
                    || line.contains("use ax_sync")
                    || line.contains("ax_task::sync")
                    || line.contains("ax_runtime::sync"))
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

fn line_is_in_internal_use_tree(line: &str, depth: &mut usize) -> bool {
    let starts_internal_tree = *depth == 0 && starts_internal_use_tree(line);
    let in_internal_tree = *depth != 0 || starts_internal_tree;
    if !in_internal_tree {
        return false;
    }

    let opens = line.bytes().filter(|byte| *byte == b'{').count();
    let closes = line.bytes().filter(|byte| *byte == b'}').count();
    *depth = depth.saturating_add(opens).saturating_sub(closes);
    in_internal_tree
}

fn starts_internal_use_tree(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(use_index) = trimmed.find("use ") else {
        return false;
    };
    if use_index != 0 && !trimmed[..use_index].starts_with("pub") {
        return false;
    }

    let path = &trimmed[use_index + "use ".len()..];
    path.contains('{')
        && ["crate::", "self::", "super::"]
            .iter()
            .any(|root| path.starts_with(root))
}

fn check_runtime_providers(
    workspace_root: &Path,
    findings: &mut Vec<Finding>,
) -> anyhow::Result<()> {
    let mut runtime_counts = [0usize; PROVIDER_TRAITS.len()];

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
            let qualified = format!("impl ax_sync::interface::{trait_name} for");
            let local = format!("impl {trait_name} for");
            let occurrences =
                contents.matches(&qualified).count() + contents.matches(&local).count();
            if occurrences == 0 {
                continue;
            }

            if relative == RUNTIME_PROVIDER_PATH {
                runtime_counts[trait_index] += occurrences;
            } else {
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
    check_host_engine_cfg(workspace_root, findings)?;
    Ok(())
}

fn check_host_engine_cfg(workspace_root: &Path, findings: &mut Vec<Finding>) -> anyhow::Result<()> {
    let path = workspace_root.join(AX_SYNC_HOST_MODULE_PATH);
    if !path.exists() {
        return Ok(());
    }
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if !contents.contains("all(feature = \"host-test\", not(target_os = \"none\"))")
        || !contents.contains("mod host;")
    {
        findings.push(Finding::new(
            &path,
            "host engine cfg",
            "ax-sync host engine is not restricted to host-test on std-capable targets",
            "gate the host module with all(feature = \"host-test\", not(target_os = \"none\"))",
        ));
    }
    Ok(())
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

fn is_os_layer_manifest(workspace_root: &Path, path: &Path) -> bool {
    let relative = relative_path(workspace_root, path);
    relative.starts_with("os/arceos/api/")
        || relative.starts_with("os/arceos/modules/")
        || relative.starts_with("os/arceos/ulib/")
        || relative.starts_with("os/StarryOS/")
        || relative.starts_with("os/axvisor/")
        || relative.starts_with("virtualization/axvm/")
}

fn is_allowed_ax_sync_os_edge(workspace_root: &Path, path: &Path) -> bool {
    let relative = relative_path(workspace_root, path);
    AX_SYNC_OS_EDGE_ALLOWLIST.contains(&relative.as_str())
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
impl ax_sync::interface::ContextOps for RuntimeContextOps {}
impl ax_sync::interface::SpinOps for RuntimeSpinOps {}
impl ax_sync::interface::RwLockOps for RuntimeRwLockOps {}
impl ax_sync::interface::MutexOps for RuntimeMutexOps {}
impl ax_sync::interface::LockdepOps for RuntimeLockdepOps {}
"#,
        );
    }

    fn write_workspace_with_internal_helper(root: &Path) {
        write_minimal_workspace(root);
        write_file(
            root,
            "Cargo.toml",
            r#"
[workspace]
members = ["crate", "helper"]

[workspace.dependencies]
helper = { version = "0.1.0", path = "helper" }
"#,
        );
        write_file(
            root,
            "helper/Cargo.toml",
            r#"
[package]
name = "helper"
version = "0.1.0"
edition = "2024"
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
    fn rejects_versioned_workspace_internal_dev_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_workspace_with_internal_helper(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
helper.workspace = true
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.location == "dev-dependencies.helper"
                && finding.message.contains("must not inherit a version")
        }));
    }

    #[test]
    fn rejects_versioned_path_internal_dev_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_workspace_with_internal_helper(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
helper = { version = "0.1.0", path = "../helper" }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.location == "dev-dependencies.helper"
                && finding.message.contains("must not specify a version")
        }));
    }

    #[test]
    fn accepts_path_only_internal_and_versioned_external_dev_dependencies() {
        let root = tempfile::tempdir().unwrap();
        write_workspace_with_internal_helper(root.path());
        write_file(
            root.path(),
            "crate/Cargo.toml",
            r#"
[package]
name = "crate"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
helper = { path = "../helper" }
external = "1"
"#,
        );

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
    fn accepts_internal_spin_module_use_trees() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "crate/src/lib.rs",
            r#"
use crate::{
    mutex::RawMutex,
    spin::lockdep::LockdepMap,
};
pub use self::{
    context::Guard,
    spin::*,
};
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .all(|finding| !finding.message.contains("direct crates.io")),
            "internal spin module was mistaken for crates.io spin: {findings:?}"
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
            "impl ax_sync::interface::ContextOps for OtherRuntime {}\n",
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("outside ax-runtime"))
        );
    }

    #[test]
    fn rejects_unconditional_host_engine() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            AX_SYNC_HOST_MODULE_PATH,
            r#"
mod host;
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("host engine is not restricted to host-test")
        }));
    }

    #[test]
    fn accepts_target_aware_host_provider_selection() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            AX_SYNC_HOST_MODULE_PATH,
            r#"
#[cfg(all(feature = "host-test", not(target_os = "none")))]
mod host;
"#,
        );

        assert!(lint_workspace(root.path()).unwrap().is_empty());
    }

    #[test]
    fn rejects_removed_ax_sync_features() {
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
[features]
smp = ["ax-sync/smp"]
[dependencies]
ax-sync = { version = "0.1", features = ["lockdep"] }
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("smp"))
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("lockdep"))
        );
    }

    #[test]
    fn rejects_removed_ax_sync_test_support() {
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
ax-sync-test-support = "0.1"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("dependency on removed lock crate `ax-sync-test-support`")
        }));
    }

    #[test]
    fn rejects_os_layer_ax_sync_dependency_outside_allowlist() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "os/arceos/modules/axtask/Cargo.toml",
            r#"
[package]
name = "ax-task"
version = "0.1.0"
edition = "2024"
[dependencies]
ax-sync = "0.1"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("OS-layer crate must not depend directly")
        }));
    }

    #[test]
    fn rejects_starry_signal_domain_crate_ax_sync_dependency() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "os/StarryOS/signal/Cargo.toml",
            r#"
[package]
name = "starry-signal"
version = "0.1.0"
edition = "2024"
[dependencies]
ax-sync = "0.1"
"#,
        );

        let findings = lint_workspace(root.path()).unwrap();
        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("OS-layer crate must not depend directly")
        }));
    }

    #[test]
    fn accepts_documented_cycle_breaking_ax_sync_edge() {
        let root = tempfile::tempdir().unwrap();
        write_minimal_workspace(root.path());
        write_file(
            root.path(),
            "os/arceos/modules/axhal/Cargo.toml",
            r#"
[package]
name = "ax-hal"
version = "0.1.0"
edition = "2024"
[dependencies]
ax-sync = "0.1"
"#,
        );

        assert!(lint_workspace(root.path()).unwrap().is_empty());
    }
}
