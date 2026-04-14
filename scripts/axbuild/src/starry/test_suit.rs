use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

use super::board;
use crate::context::{arch_for_target_checked, starry_target_for_arch_checked};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryTestGroup {
    Normal,
    Stress,
}

impl StarryTestGroup {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Stress => "stress",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCase {
    pub(crate) name: String,
    pub(crate) qemu_config_path: PathBuf,
}

pub(crate) fn parse_test_target(
    workspace_root: &Path,
    target: &str,
) -> anyhow::Result<(String, String)> {
    let supported_targets = board::board_default_list(workspace_root)?
        .into_iter()
        .filter(|board| board.name.starts_with("qemu-"))
        .map(|board| board.target)
        .collect::<Vec<_>>();

    let supported_target_refs = supported_targets
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let supported_arches = supported_targets
        .iter()
        .map(|target| arch_for_target_checked(target))
        .collect::<anyhow::Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();

    if target.contains('-') {
        validate_supported_target(
            target,
            "starry qemu tests",
            "targets",
            &supported_target_refs,
        )?;
        Ok((
            arch_for_target_checked(target)?.to_string(),
            target.to_string(),
        ))
    } else {
        validate_supported_target(
            target,
            "starry qemu tests",
            "arch values",
            &supported_arches,
        )?;
        Ok((
            target.to_string(),
            starry_target_for_arch_checked(target)?.to_string(),
        ))
    }
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    arch: &str,
    selected_case: Option<&str>,
    group: StarryTestGroup,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group);
    let config_name = qemu_config_name(arch);

    if let Some(case_name) = selected_case {
        let case_dir = test_suite_dir.join(case_name);
        if !case_dir.is_dir() {
            bail!(
                "unknown Starry {} test case `{case_name}` in {}; available cases are discovered \
                 from direct subdirectories",
                group.as_str(),
                test_suite_dir.display()
            );
        }

        let qemu_config_path = case_dir.join(&config_name);
        if !qemu_config_path.is_file() {
            bail!(
                "Starry test case `{case_name}` does not provide `{}`",
                qemu_config_path.display()
            );
        }

        return Ok(vec![StarryQemuCase {
            name: case_name.to_string(),
            qemu_config_path,
        }]);
    }

    let mut cases = fs::read_dir(&test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }

            let name = entry.file_name().into_string().ok()?;
            let qemu_config_path = path.join(&config_name);
            if qemu_config_path.is_file() {
                Some(StarryQemuCase {
                    name,
                    qemu_config_path,
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    cases.sort_by(|left, right| left.name.cmp(&right.name));

    if cases.is_empty() {
        bail!(
            "no Starry {} qemu test cases for arch `{arch}` found under {}",
            group.as_str(),
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

pub(crate) fn finalize_qemu_case_run(
    failed: &[String],
    group: StarryTestGroup,
) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all starry {} qemu test cases passed", group.as_str());
        Ok(())
    } else {
        bail!(
            "starry {} qemu tests failed for {} case(s): {}",
            group.as_str(),
            failed.len(),
            failed.join(", ")
        )
    }
}

fn validate_supported_target(
    target: &str,
    suite_name: &str,
    supported_kind: &str,
    supported: &[&str],
) -> anyhow::Result<()> {
    if supported.contains(&target) {
        Ok(())
    } else {
        bail!(
            "unsupported target `{}` for {}. Supported {} are: {}",
            target,
            suite_name,
            supported_kind,
            supported.join(", ")
        )
    }
}

fn test_suite_dir(workspace_root: &Path, group: StarryTestGroup) -> PathBuf {
    workspace_root
        .join("test-suit")
        .join("starryos")
        .join(group.as_str())
}

fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}
