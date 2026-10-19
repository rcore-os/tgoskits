use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

pub(crate) fn suite_root(workspace_root: &Path, os_name: &str) -> PathBuf {
    workspace_root.join("test-suit").join(os_name)
}

pub(crate) fn group_dir(workspace_root: &Path, os_name: &str, group: &str) -> PathBuf {
    suite_root(workspace_root, os_name).join(group)
}

pub(crate) fn discover_group_names(
    workspace_root: &Path,
    os_name: &str,
) -> anyhow::Result<Vec<String>> {
    discover_group_names_in_root(&suite_root(workspace_root, os_name))
}

/// List the group directories directly under a suite root.
///
/// Callers select their application or test namespace before using this
/// listing; it does not combine namespaces.
pub(crate) fn discover_group_names_in_root(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut groups = Vec::new();
    if root.is_dir() {
        for entry in
            fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))?
        {
            let entry = entry?;
            if entry.path().is_dir()
                && let Ok(name) = entry.file_name().into_string()
            {
                groups.push(name);
            }
        }
    }
    groups.sort();
    Ok(groups)
}

pub(crate) fn supported_group_names(
    workspace_root: &Path,
    os_name: &str,
) -> anyhow::Result<String> {
    let groups = discover_group_names(workspace_root, os_name)?;
    Ok(if groups.is_empty() {
        "<none>".to_string()
    } else {
        groups.join(", ")
    })
}
