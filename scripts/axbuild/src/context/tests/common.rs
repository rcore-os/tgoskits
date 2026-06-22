pub(super) use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use ostool::invocation::{Invocation, InvocationOptions};
pub(super) use tempfile::tempdir;

use super::*;

pub(super) fn test_app_context(root: &Path) -> AppContext {
    AppContext {
        invocation: test_invocation(root),
        build_config_path: None,
        root: root.to_path_buf(),
        member_dirs: HashMap::from([("axvisor".to_string(), root.join("os/axvisor"))]),
        original_path: env::var_os("PATH").unwrap_or_default(),
        debug: false,
    }
}

fn test_invocation(root: &Path) -> Invocation {
    let manifest_path = root.join("Cargo.toml");
    if !manifest_path.exists() {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(
            &manifest_path,
            r#"[package]
name = "test-workspace"
version = "0.1.0"
edition = "2021"

[workspace]
"#,
        )
        .unwrap();
    }
    Invocation::new(InvocationOptions::new(
        Some(manifest_path),
        None,
        None,
        false,
    ))
    .unwrap()
}

fn resolve_arceos_build_info_path(
    package: &str,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    crate::arceos::build::resolve_build_info_path(package, target, explicit_path)
}

pub(super) fn prepare_arceos_request(
    app: &AppContext,
    cli: BuildCliArgs,
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
    app.prepare_arceos_request(
        cli,
        qemu_config,
        uboot_config,
        resolve_arceos_build_info_path,
    )
}

fn resolve_starry_build_info_path(
    workspace_root: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    crate::starry::build::resolve_build_info_path(workspace_root, target, explicit_path)
}

pub(super) fn prepare_starry_request(
    app: &AppContext,
    cli: StarryCliArgs,
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
    app.prepare_starry_request(
        cli,
        qemu_config,
        uboot_config,
        resolve_starry_build_info_path,
    )
}

pub(super) fn prepare_axvisor_request(
    app: &AppContext,
    cli: AxvisorCliArgs,
    qemu_config: Option<PathBuf>,
    uboot_config: Option<PathBuf>,
) -> anyhow::Result<(ResolvedAxvisorRequest, AxvisorCommandSnapshot)> {
    app.prepare_axvisor_request(
        cli,
        AxvisorRequestPaths {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: app.root.join("os/axvisor"),
            load_config_target: crate::axvisor::build::load_target_from_build_config,
            resolve_build_info_path: crate::axvisor::build::resolve_build_info_path,
        },
        qemu_config,
        uboot_config,
    )
}

pub(super) static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub(super) struct TempEnvVar {
    key: &'static str,
    original: Option<OsString>,
}

impl TempEnvVar {
    pub(super) fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    pub(super) fn unset(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for TempEnvVar {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(value) => unsafe {
                env::set_var(self.key, value);
            },
            None => unsafe {
                env::remove_var(self.key);
            },
        }
    }
}

fn write_minimal_workspace_package(path: &Path, name: &str) {
    let src_dir = path.parent().unwrap().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("lib.rs"), "").unwrap();
    fs::write(
        path,
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
}

pub(super) fn prepare_starry_workspace(root: &Path) {
    let starry_dir = root.join("os/StarryOS/starryos");
    fs::create_dir_all(&starry_dir).unwrap();
    write_minimal_workspace_package(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
    )
    .unwrap();
}

pub(super) fn snapshot_path(root: &Path, file_name: &str) -> PathBuf {
    axbuild_tmp_dir(root).join(file_name)
}

pub(super) fn write_snapshot_text(
    root: &Path,
    file_name: &str,
    content: &str,
) -> std::io::Result<()> {
    let path = snapshot_path(root, file_name);
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, content)
}
