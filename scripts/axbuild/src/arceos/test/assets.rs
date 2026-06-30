use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

use super::{ARCEOS_C_TEST_GROUP, ARCEOS_RUST_TEST_GROUP, ARCEOS_TEST_SUITE_OS};
use crate::{
    arceos::ArceOS,
    context::BuildCliArgs,
    test::{qemu as qemu_test, suite as test_suite},
};

pub(super) fn test_build_args(package: &str, target: &str, config: &Path) -> BuildCliArgs {
    BuildCliArgs {
        config: Some(config.to_path_buf()),
        package: Some(package.to_string()),
        arch: None,
        target: Some(target.to_string()),
        plat_dyn: None,
        smp: None,
        debug: false,
    }
}

pub(super) fn arceos_rust_test_dir(arceos: &ArceOS) -> PathBuf {
    arceos_test_group_dir(arceos.app.workspace_root(), ARCEOS_RUST_TEST_GROUP)
}

pub(super) fn arceos_c_test_dir(arceos: &ArceOS) -> PathBuf {
    arceos_test_group_dir(arceos.app.workspace_root(), ARCEOS_C_TEST_GROUP)
}

pub(super) fn arceos_test_group_dir(workspace_root: &Path, group: &str) -> PathBuf {
    test_suite::group_dir(workspace_root, ARCEOS_TEST_SUITE_OS, group)
}

pub(super) fn read_manifest_package_name(path: &Path) -> anyhow::Result<String> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    value
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("missing package.name in {}", path.display()))
}

pub(super) fn arceos_test_suit_qemu_archs(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut archs = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Some(arch) = stem.strip_prefix("qemu-")
            && !arch.starts_with("base-")
        {
            archs.push(arch.to_string());
        }
    }
    archs.sort();
    Ok(archs)
}

pub(super) fn qemu_config_path(
    root: &Path,
    arch: &str,
    suite_name: &str,
) -> anyhow::Result<PathBuf> {
    let path = root.join(qemu_test::qemu_config_name(arch));
    if path.is_file() {
        return Ok(path);
    }
    anyhow::bail!("{suite_name} must provide {}", path.display())
}

pub(super) fn build_config_path(
    root: &Path,
    target: &str,
    suite_name: &str,
) -> anyhow::Result<PathBuf> {
    let path = root.join(format!("build-{target}.toml"));
    if path.is_file() {
        return Ok(path);
    }
    anyhow::bail!("{suite_name} must provide {}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arceos::test::ARCEOS_RUST_TEST_PACKAGE;

    #[test]
    fn arceos_rust_qemu_test_uses_single_test_suite_package() {
        let app_dir = tempfile::tempdir().unwrap();
        let build_config = app_dir.path().join("build-x86_64-unknown-none.toml");
        fs::write(&build_config, "features = [\"ax-std\"]\n").unwrap();

        let args = test_build_args(
            ARCEOS_RUST_TEST_PACKAGE,
            "x86_64-unknown-none",
            &build_config,
        );

        assert_eq!(args.config, Some(build_config));
        assert_eq!(args.package.as_deref(), Some(ARCEOS_RUST_TEST_PACKAGE));
        assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
    }
}
