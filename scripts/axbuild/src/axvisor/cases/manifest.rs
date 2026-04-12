use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::Deserialize;

pub(crate) const AXVISOR_TEST_SUITE_ROOT: &str = "test-suit/axvisor";
const CASE_MANIFEST_FILE: &str = "case.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadedCase {
    pub(crate) case_dir: PathBuf,
    pub(crate) manifest: CaseManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct SuiteManifest {
    pub(crate) name: String,
    pub(crate) arches: BTreeMap<String, SuiteArchManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct SuiteArchManifest {
    pub(crate) cases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct CaseManifest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) arch: Vec<String>,
    pub(crate) timeout_secs: u64,
    #[serde(default)]
    pub(crate) description: Option<String>,
}

pub(crate) fn load_cases_from_suite(
    workspace_root: &Path,
    suite_path: &Path,
    arch: &str,
) -> anyhow::Result<(SuiteManifest, Vec<LoadedCase>)> {
    let suite = load_suite_manifest(suite_path)?;
    let arch_entry = suite.arches.get(arch).ok_or_else(|| {
        anyhow!(
            "suite `{}` does not define cases for arch `{}`",
            suite.name,
            arch
        )
    })?;

    let suite_root = workspace_root.join(AXVISOR_TEST_SUITE_ROOT);
    let mut cases = Vec::with_capacity(arch_entry.cases.len());
    for case_ref in &arch_entry.cases {
        let case_dir = suite_root.join(case_ref);
        let loaded = load_case_from_dir(&case_dir).with_context(|| {
            format!(
                "failed to load case `{}` referenced by suite `{}`",
                case_ref, suite.name
            )
        })?;
        ensure_case_supports_arch(&loaded, arch)?;
        cases.push(loaded);
    }

    Ok((suite, cases))
}

pub(crate) fn load_case_from_dir(case_dir: &Path) -> anyhow::Result<LoadedCase> {
    let manifest_path = case_dir.join(CASE_MANIFEST_FILE);
    let manifest = load_case_manifest(&manifest_path)?;
    Ok(LoadedCase {
        case_dir: case_dir.to_path_buf(),
        manifest,
    })
}

fn load_suite_manifest(path: &Path) -> anyhow::Result<SuiteManifest> {
    let manifest: SuiteManifest = read_toml(path)?;
    if manifest.arches.is_empty() {
        bail!(
            "suite manifest {} has no [arches.*] entries",
            path.display()
        );
    }
    Ok(manifest)
}

fn load_case_manifest(path: &Path) -> anyhow::Result<CaseManifest> {
    let manifest: CaseManifest = read_toml(path)?;
    validate_case_manifest(&manifest, path)?;
    Ok(manifest)
}

fn validate_case_manifest(manifest: &CaseManifest, path: &Path) -> anyhow::Result<()> {
    if manifest.id.trim().is_empty() {
        bail!("case manifest {} has empty `id`", path.display());
    }
    if manifest.arch.is_empty() {
        bail!(
            "case manifest {} must declare at least one arch",
            path.display()
        );
    }
    if manifest.timeout_secs == 0 {
        bail!(
            "case manifest {} must set `timeout_secs` > 0",
            path.display()
        );
    }
    Ok(())
}

pub(crate) fn ensure_case_supports_arch(case: &LoadedCase, arch: &str) -> anyhow::Result<()> {
    if case.manifest.arch.iter().any(|value| value == arch) {
        Ok(())
    } else {
        bail!(
            "case `{}` at {} does not support arch `{}`",
            case.manifest.id,
            case.case_dir.display(),
            arch
        )
    }
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn load_case_manifest_requires_timeout() {
        let dir = tempdir().unwrap();
        let case_dir = dir.path().join("case");
        fs::create_dir_all(&case_dir).unwrap();

        fs::write(
            case_dir.join(CASE_MANIFEST_FILE),
            r#"
id = "example.pass"
arch = ["aarch64"]
timeout_secs = 15
description = "example case"
"#,
        )
        .unwrap();

        let loaded = load_case_from_dir(&case_dir).unwrap();
        assert_eq!(loaded.manifest.id, "example.pass");
        assert_eq!(loaded.manifest.timeout_secs, 15);
        assert_eq!(loaded.manifest.description.as_deref(), Some("example case"));
    }

    #[test]
    fn load_case_manifest_rejects_zero_timeout() {
        let dir = tempdir().unwrap();
        let case_dir = dir.path().join("case");
        fs::create_dir_all(&case_dir).unwrap();

        fs::write(
            case_dir.join(CASE_MANIFEST_FILE),
            r#"
id = "example.pass"
arch = ["aarch64"]
timeout_secs = 0
"#,
        )
        .unwrap();

        assert!(load_case_from_dir(&case_dir).is_err());
    }

    #[test]
    fn load_suite_manifest_resolves_selected_arch_cases() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let suite_root = workspace_root.join(AXVISOR_TEST_SUITE_ROOT);
        let case_dir = suite_root.join("example/pass-report");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(
            case_dir.join(CASE_MANIFEST_FILE),
            r#"
id = "example.pass"
arch = ["aarch64", "x86_64"]
timeout_secs = 5
"#,
        )
        .unwrap();

        let suites_dir = suite_root.join("suites");
        fs::create_dir_all(&suites_dir).unwrap();
        let suite_path = suites_dir.join("examples.toml");
        fs::write(
            &suite_path,
            r#"
name = "examples"

[arches.aarch64]
cases = ["example/pass-report"]
"#,
        )
        .unwrap();

        let (suite, cases) = load_cases_from_suite(workspace_root, &suite_path, "aarch64").unwrap();
        assert_eq!(suite.name, "examples");
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].manifest.id, "example.pass");
    }
}
