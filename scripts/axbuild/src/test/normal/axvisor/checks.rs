//! Caller-owned checks, prerequisites and ordered runtime expansion.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckReferences {
    pub(crate) id: Option<String>,
    pub(crate) checks: Option<Vec<PathBuf>>,
}

pub(crate) struct CheckFile {
    pub(crate) path: PathBuf,
    pub(crate) document: toml::Table,
}

pub(crate) struct RuntimeChecks {
    pub(crate) references: CheckReferences,
    pub(crate) runtime: toml::Table,
    pub(crate) checks: Vec<CheckFile>,
}

pub(crate) fn runtime_check_references(path: &Path) -> anyhow::Result<CheckReferences> {
    let source: toml::Table = toml::from_str(&fs::read_to_string(path)?)?;
    source
        .get("axbuild")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()
        .with_context(|| format!("invalid check references in {}", path.display()))
        .map(Option::unwrap_or_default)
}

/// Read the ordered public member names without running or preparing anything.
pub(crate) fn runtime_check_names(path: &Path) -> anyhow::Result<Vec<String>> {
    let refs = runtime_check_references(path)?;
    let mut seen = BTreeSet::new();
    refs.checks
        .unwrap_or_default()
        .iter()
        .map(|file| {
            let name = file
                .file_stem()
                .and_then(|name| name.to_str())
                .context("check path has no UTF-8 file name")?
                .to_string();
            ensure!(
                seen.insert(name.clone()),
                "duplicate check name `{name}` in {}",
                path.display()
            );
            Ok(name)
        })
        .collect()
}

/// Select boot groups or named members; short member names must be unambiguous.
pub(crate) fn select_runtime_members(
    groups: &[(&str, &Path)],
    selectors: &[String],
) -> anyhow::Result<Vec<(usize, Option<Vec<String>>)>> {
    let mut chosen = std::collections::BTreeMap::<usize, Option<Vec<String>>>::new();
    if selectors.is_empty() {
        return Ok((0..groups.len()).map(|index| (index, None)).collect());
    }
    for selector in selectors {
        let mut matches = Vec::new();
        for (index, (group, path)) in groups.iter().enumerate() {
            if *group == selector
                || group
                    .strip_prefix(selector.as_str())
                    .is_some_and(|tail| tail.starts_with('/'))
            {
                matches.push((index, None));
                continue;
            }
            for name in runtime_check_names(path)? {
                if name == *selector || format!("{group}/{name}") == *selector {
                    matches.push((index, Some(name)));
                }
            }
        }
        ensure!(!matches.is_empty(), "unknown test case `{selector}`");
        let short = matches
            .iter()
            .filter(|(_, member)| member.is_some())
            .count();
        ensure!(
            short <= 1,
            "ambiguous test case `{selector}`; qualify it with the boot group path"
        );
        for (index, member) in matches {
            match member {
                None => {
                    chosen.insert(index, None);
                }
                Some(name) => {
                    let members = chosen.entry(index).or_insert_with(|| Some(Vec::new()));
                    if let Some(members) = members
                        && !members.contains(&name)
                    {
                        members.push(name);
                    }
                }
            }
        }
    }
    Ok(chosen.into_iter().collect())
}

pub(crate) fn load_runtime_checks(path: &Path) -> anyhow::Result<RuntimeChecks> {
    load_selected_runtime_checks(path, None)
}

/// Resolve only selected members and their explicit prerequisites, in declaration order.
pub(crate) fn load_selected_runtime_checks(
    path: &Path,
    selected: Option<&[String]>,
) -> anyhow::Result<RuntimeChecks> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read runtime configuration {}", path.display()))?;
    let mut runtime: toml::Table = toml::from_str(&content)
        .with_context(|| format!("parse runtime configuration {}", path.display()))?;
    let references: CheckReferences = runtime
        .remove("axbuild")
        .map(toml::Value::try_into)
        .transpose()
        .with_context(|| format!("invalid axbuild check references in {}", path.display()))?
        .unwrap_or_default();
    if let Some(id) = &references.id {
        crate::execution::qemu::validate_selected_case_name(id, "declared", "runtime")?;
        ensure!(
            !id.trim().is_empty() && id.trim() == id,
            "invalid check group ID in {}",
            path.display()
        );
    }
    let names = runtime_check_names(path)?;
    if let Some(selected) = selected {
        ensure!(
            !selected.is_empty(),
            "empty check selection in {}",
            path.display()
        );
        for name in selected {
            ensure!(
                names.contains(name),
                "unknown check `{name}` in {}",
                path.display()
            );
        }
    }
    let mut checks = Vec::new();
    if let Some(paths) = &references.checks {
        ensure!(!paths.is_empty(), "empty check list in {}", path.display());
        ensure!(
            !runtime.contains_key("shell_check_steps") && !runtime.contains_key("host_http_probe"),
            "{} cannot mix inline checks and external check references",
            path.display()
        );
        let mut declared = BTreeSet::new();
        let mut completed = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        for (relative, name) in paths.iter().zip(&names) {
            let file = path
                .parent()
                .context("runtime configuration has no parent")?
                .join(relative);
            if selected.is_none_or(|selected| selected.contains(name)) {
                let canonical = file
                    .canonicalize()
                    .with_context(|| format!("resolve check {}", file.display()))?;
                ensure!(
                    declared.insert(canonical),
                    "duplicate check reference {}",
                    file.display()
                );
                expand_check(&file, &mut visiting, &mut completed, &mut checks)?;
            }
        }
        if selected.is_some()
            && let Some(budgets) = checks
                .iter()
                .map(|check| {
                    check
                        .document
                        .get("timeout")
                        .and_then(toml::Value::as_integer)
                })
                .collect::<Option<Vec<_>>>()
        {
            let budget = budgets.into_iter().try_fold(0_i64, |total, budget| {
                ensure!(budget > 0, "check timeout must be positive");
                total
                    .checked_add(budget)
                    .context("selected check timeout budget overflow")
            })?;
            let budget = runtime
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .map_or(budget, |group_budget| group_budget.min(budget));
            runtime.insert("timeout".into(), toml::Value::Integer(budget));
        }
        let mut steps = Vec::new();
        for check in &checks {
            if let Some(value) = check.document.get("shell_check_steps") {
                steps.extend(
                    value
                        .as_array()
                        .context("shell_check_steps must be an array")?
                        .iter()
                        .cloned(),
                );
            }
        }
        if !steps.is_empty() {
            runtime.insert("shell_check_steps".into(), toml::Value::Array(steps));
        }
    }
    Ok(RuntimeChecks {
        references,
        runtime,
        checks,
    })
}

fn expand_check(
    path: &Path,
    visiting: &mut BTreeSet<PathBuf>,
    completed: &mut BTreeSet<PathBuf>,
    checks: &mut Vec<CheckFile>,
) -> anyhow::Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve check {}", path.display()))?;
    if completed.contains(&canonical) {
        return Ok(());
    }
    ensure!(
        visiting.insert(canonical.clone()),
        "cyclic check prerequisite {}",
        path.display()
    );
    let document: toml::Table = toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("read check {}", path.display()))?;
    for field in document.keys() {
        ensure!(
            [
                "id",
                "name",
                "description",
                "timeout",
                "requires",
                "shell_check_steps",
                "host_http_probe"
            ]
            .contains(&field.as_str()),
            "unsupported check field `{field}` in {}",
            path.display()
        );
    }
    if let Some(timeout) = document.get("timeout") {
        ensure!(
            timeout.as_integer().is_some_and(|t| t > 0),
            "check timeout must be a positive integer in {}",
            path.display()
        );
    }
    if let Some(required) = document.get("requires") {
        let required: Vec<PathBuf> = required.clone().try_into()?;
        for file in required {
            expand_check(
                &path.parent().context("check has no parent")?.join(file),
                visiting,
                completed,
                checks,
            )?;
        }
    }
    ensure!(
        document.contains_key("shell_check_steps") || document.contains_key("host_http_probe"),
        "{} defines no executable checks",
        path.display()
    );
    if let Some(value) = document.get("shell_check_steps") {
        let decoded: Vec<ostool::run::ShellCheckStep> = value.clone().try_into()?;
        ensure!(
            !decoded.is_empty(),
            "{} has an empty shell sequence",
            path.display()
        );
    }
    visiting.remove(&canonical);
    completed.insert(canonical);
    checks.push(CheckFile {
        path: path.to_path_buf(),
        document,
    });
    Ok(())
}

/// Keep the expanded configuration alive while the runner resolves variables.
pub(crate) fn materialize_selected_runtime_checks(
    path: &Path,
    directory: &Path,
    selected: Option<&[String]>,
) -> anyhow::Result<Option<tempfile::NamedTempFile>> {
    let plan = load_selected_runtime_checks(path, selected)?;
    if plan.references.checks.is_none() && plan.references.id.is_none() {
        return Ok(None);
    }
    if !plan.checks.is_empty() {
        println!(
            "[axbuild] ordered checks: {}",
            plan.checks
                .iter()
                .filter_map(|check| check.path.file_stem().and_then(|name| name.to_str()))
                .collect::<Vec<_>>()
                .join(" -> ")
        );
    }
    fs::create_dir_all(directory)?;
    let file = tempfile::Builder::new()
        .prefix("runtime-")
        .suffix(".toml")
        .tempfile_in(directory)?;
    fs::write(file.path(), toml::to_string(&plan.runtime)?)?;
    Ok(Some(file))
}

pub(crate) fn materialize_runtime_checks(
    path: &Path,
    directory: &Path,
) -> anyhow::Result<Option<tempfile::NamedTempFile>> {
    materialize_selected_runtime_checks(path, directory, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_selection_preserves_shared_prerequisites_and_rejects_invalid_selection() {
        let root = tempfile::tempdir().unwrap();
        let write =
            |name: &str, contents: &str| fs::write(root.path().join(name), contents).unwrap();
        write(
            "runtime.toml",
            "timeout=300\n[axbuild]\nchecks=['first.toml','second.toml','broken.toml']\n",
        );
        write(
            "enter.toml",
            "timeout=10\n[[shell_check_steps]]\nshell_cmd='enter guest'\n",
        );
        write(
            "first.toml",
            r#"timeout=20
requires=['enter.toml']
[[shell_check_steps]]
shell_cmd='first'
success_regex=['FIRST']
"#,
        );
        write(
            "second.toml",
            r#"timeout=30
requires=['enter.toml']
[[shell_check_steps]]
shell_cmd='second'
success_regex=['SECOND']
"#,
        );
        write("broken.toml", "invalid=[");
        let config = root.path().join("runtime.toml");
        let one = load_selected_runtime_checks(&config, Some(&["second".into()])).unwrap();
        assert_eq!(one.checks.len(), 2);
        assert_eq!(one.runtime["timeout"].as_integer(), Some(40));
        assert_eq!(
            one.runtime["shell_check_steps"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            one.runtime["shell_check_steps"][1]["shell_cmd"].as_str(),
            Some("second")
        );
        let both = load_selected_runtime_checks(
            &config,
            Some(&["second".into(), "first".into(), "first".into()]),
        )
        .unwrap();
        let commands = both.runtime["shell_check_steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["shell_cmd"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(commands, ["enter guest", "first", "second"]);
        assert_eq!(both.runtime["timeout"].as_integer(), Some(60));
        assert!(load_selected_runtime_checks(&config, Some(&[])).is_err());
        assert!(load_selected_runtime_checks(&config, Some(&["missing".into()])).is_err());
        assert!(load_runtime_checks(&config).is_err());
        let descriptors = [("environment", config.as_path())];
        let selection =
            select_runtime_members(&descriptors, &["second".into(), "environment/first".into()])
                .unwrap();
        assert_eq!(
            selection,
            [(0, Some(vec!["second".into(), "first".into()]))]
        );
        let all =
            select_runtime_members(&descriptors, &["second".into(), "environment".into()]).unwrap();
        assert_eq!(all, [(0, None)]);
        assert!(
            select_runtime_members(
                &[("a", config.as_path()), ("b", config.as_path())],
                &["second".into()]
            )
            .is_err()
        );
        write(
            "enter.toml",
            "requires=['second.toml']\n[[shell_check_steps]]\nshell_cmd='enter'\n",
        );
        assert!(
            load_selected_runtime_checks(&config, Some(&["second".into()]))
                .err()
                .unwrap()
                .to_string()
                .contains("cyclic")
        );
    }

    #[test]
    fn explicit_references_preserve_order_and_reject_incomplete_or_ambiguous_inputs() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("b.toml"),
            "[[shell_check_steps]]\nshell_cmd='second'\nsuccess_regex=['SECOND']\n",
        )
        .unwrap();
        fs::write(
            root.path().join("a.toml"),
            "[[shell_check_steps]]\nshell_cmd='first'\nsuccess_regex=['FIRST']\n",
        )
        .unwrap();
        let config = root.path().join("machine.toml");
        fs::write(
            &config,
            "args=[]\n[axbuild]\nid='synthetic'\nchecks=['a.toml','b.toml']\n",
        )
        .unwrap();
        let plan = load_runtime_checks(&config).unwrap();
        let steps = plan.runtime["shell_check_steps"].as_array().unwrap();
        assert_eq!(steps[0]["shell_cmd"].as_str(), Some("first"));
        assert_eq!(steps[1]["shell_cmd"].as_str(), Some("second"));
        for invalid in [
            "checks=[]",
            "checks=['missing.toml']",
            "checks=['a.toml','a.toml']",
        ] {
            fs::write(&config, format!("[axbuild]\n{invalid}\n")).unwrap();
            assert!(load_runtime_checks(&config).is_err());
        }
        fs::write(
            &config,
            "shell_check_steps=[]\n[axbuild]\nchecks=['a.toml']\n",
        )
        .unwrap();
        assert!(load_runtime_checks(&config).is_err());
    }
}
