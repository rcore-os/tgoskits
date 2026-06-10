use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use anyhow::Context;
use log::info;
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::{CargoQemuRunnerArgs, CargoRunnerKind, CargoUbootRunnerArgs, config::Cargo},
    invocation::{Invocation, InvocationOptions},
    run::{
        qemu::{QemuConfig, RunQemuOptions},
        uboot::UbootConfig,
    },
};

mod arch;
mod resolve;
mod snapshot;
#[cfg(test)]
mod tests;
mod types;
mod workspace;

pub(crate) use arch::{
    CrossCompileSpec, arch_for_target_checked, cross_compile_spec_for_arch_checked,
    default_rootfs_image_for_arch, resolve_arceos_arch_and_target, resolve_axvisor_arch_and_target,
    resolve_starry_arch_and_target, starry_arch_for_target_checked,
    starry_default_platform_for_arch_checked, starry_target_for_arch_checked, supported_arches,
    supported_targets, validate_supported_target,
};
pub(crate) use resolve::{AxvisorRequestPaths, snapshot_path_value};
pub use types::{
    ARCEOS_SNAPSHOT_FILE, AXVISOR_SNAPSHOT_FILE, ArceosCommandSnapshot, ArceosQemuSnapshot,
    ArceosUbootSnapshot, AxvisorCliArgs, AxvisorCommandSnapshot, AxvisorQemuSnapshot,
    AxvisorUbootSnapshot, BuildCliArgs, DEFAULT_ARCEOS_ARCH, DEFAULT_ARCEOS_TARGET,
    DEFAULT_AXVISOR_ARCH, DEFAULT_AXVISOR_TARGET, DEFAULT_STARRY_ARCH, DEFAULT_STARRY_TARGET,
    ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest, STARRY_PACKAGE,
    STARRY_SNAPSHOT_FILE, StarryCliArgs, StarryCommandSnapshot, StarryQemuSnapshot,
    StarryUbootSnapshot,
};
pub(crate) use workspace::{
    axbuild_tmp_dir, find_workspace_root, workspace_manifest_path,
    workspace_member_dir as resolve_workspace_member_dir, workspace_metadata_root_manifest,
    workspace_metadata_root_manifest_with_deps, workspace_root_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotPersistence {
    Discard,
    Store,
}

const NO_SNAPSHOT_ENV: &str = "AXBUILD_NO_SNAPSHOT";

impl SnapshotPersistence {
    pub(crate) fn should_store(self) -> bool {
        matches!(self, Self::Store) && !snapshot_store_disabled()
    }
}

fn snapshot_store_disabled() -> bool {
    std::env::var_os(NO_SNAPSHOT_ENV)
        .as_deref()
        .is_some_and(|value| !value.is_empty() && value != OsStr::new("0"))
}

pub struct AppContext {
    invocation: Invocation,
    build_config_path: Option<PathBuf>,
    root: PathBuf,
    member_dirs: HashMap<String, PathBuf>,
    original_path: OsString,
    debug: bool,
}

impl AppContext {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let workspace_root = find_workspace_root();
        crate::support::logging::init_logging(&workspace_root)?;

        info!("Workspace root: {}", workspace_root.display());

        let invocation = Self::new_invocation(None, false)?;
        Ok(Self {
            invocation,
            build_config_path: None,
            root: workspace_root,
            member_dirs: HashMap::new(),
            original_path: env::var_os("PATH").unwrap_or_default(),
            debug: false,
        })
    }

    pub(crate) fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn workspace_member_dir(&mut self, package: &str) -> anyhow::Result<&Path> {
        if !self.member_dirs.contains_key(package) {
            let member_dir = resolve_workspace_member_dir(package)?;
            info!(
                "Workspace member dir for {package}: {}",
                member_dir.display()
            );
            self.member_dirs.insert(package.to_string(), member_dir);
        }

        self.member_dirs
            .get(package)
            .map(PathBuf::as_path)
            .context("workspace member dir should be initialized")
    }

    pub(crate) async fn build(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        ostool::build::cargo_build(
            &mut self.invocation,
            &cargo,
            self.build_config_path.as_deref(),
        )
        .await
    }

    pub(crate) async fn prepare_elf_artifact(
        &mut self,
        elf_path: PathBuf,
        to_bin: bool,
    ) -> anyhow::Result<()> {
        self.invocation.prepare_elf_artifact(elf_path, to_bin).await
    }

    pub(crate) async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        mut qemu: Option<QemuConfig>,
    ) -> anyhow::Result<()> {
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        let _path_guard = self.scoped_qemu_path(&cargo)?;
        if let Some(qemu) = qemu.as_mut() {
            crate::test::qemu::apply_x86_64_kvm_accel_if_available(qemu, &cargo);
        }
        self.set_build_config_path(build_config_path);
        ostool::build::cargo_run(
            &mut self.invocation,
            &cargo,
            self.build_config_path.as_deref(),
            &CargoRunnerKind::Qemu(Box::new(CargoQemuRunnerArgs {
                qemu,
                debug: self.debug,
                dtb_dump: false,
            })),
        )
        .await
    }

    pub(crate) async fn run_qemu(
        &mut self,
        cargo: &Cargo,
        qemu: QemuConfig,
        capture_backtrace: Option<crate::backtrace::BacktraceQemuCapture>,
    ) -> anyhow::Result<()> {
        let _path_guard = self.scoped_qemu_path(cargo)?;
        let _backtrace_capture = capture_backtrace
            .as_ref()
            .map(crate::support::backtrace_output_capture::BacktraceOutputCaptureGuard::install)
            .transpose()
            .context("failed to install backtrace block output capture")?;
        ostool::run::qemu::run_qemu(
            &mut self.invocation,
            &qemu,
            RunQemuOptions { dtb_dump: false },
        )
        .await
    }

    pub(crate) async fn run_prepared_qemu(
        &mut self,
        qemu: QemuConfig,
        capture_backtrace: Option<crate::backtrace::BacktraceQemuCapture>,
    ) -> anyhow::Result<()> {
        let _backtrace_capture = capture_backtrace
            .as_ref()
            .map(crate::support::backtrace_output_capture::BacktraceOutputCaptureGuard::install)
            .transpose()
            .context("failed to install backtrace block output capture")?;
        ostool::run::qemu::run_qemu(
            &mut self.invocation,
            &qemu,
            RunQemuOptions { dtb_dump: false },
        )
        .await
    }

    pub(crate) async fn run_prepared_uboot(&mut self, uboot: UbootConfig) -> anyhow::Result<()> {
        ostool::run::uboot::run_uboot(&mut self.invocation, &uboot).await
    }

    pub(crate) async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot: Option<UbootConfig>,
    ) -> anyhow::Result<()> {
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        self.set_build_config_path(build_config_path);
        ostool::build::cargo_run(
            &mut self.invocation,
            &cargo,
            self.build_config_path.as_deref(),
            &CargoRunnerKind::Uboot(Box::new(CargoUbootRunnerArgs { uboot })),
        )
        .await
    }

    pub(crate) async fn board(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        board_config: BoardRunConfig,
        options: RunBoardOptions,
    ) -> anyhow::Result<()> {
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        self.set_build_config_path(build_config_path);
        ostool::board::cargo_run_board(
            &mut self.invocation,
            &cargo,
            self.build_config_path.as_deref(),
            &board_config,
            options,
        )
        .await
    }

    pub(crate) fn set_debug_mode(&mut self, debug: bool) -> anyhow::Result<()> {
        if self.debug == debug {
            return Ok(());
        }

        self.invocation = Self::new_invocation(self.build_config_path.clone(), debug)?;
        self.debug = debug;

        Ok(())
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path);
    }

    fn new_invocation(manifest: Option<PathBuf>, debug: bool) -> anyhow::Result<Invocation> {
        Invocation::new(InvocationOptions::new(manifest, None, None, debug))
            .context("failed to initialize ostool invocation")
    }

    pub(crate) async fn read_qemu_config_from_path_for_cargo(
        &self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<QemuConfig> {
        ostool::run::qemu::read_config_from_path_for_cargo(&self.invocation, cargo, path).await
    }

    pub(crate) async fn read_uboot_config_from_path_for_cargo(
        &self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<UbootConfig> {
        ostool::run::uboot::read_config_from_path_for_cargo(&self.invocation, cargo, path).await
    }

    pub(crate) async fn read_board_run_config_from_path_for_cargo(
        &self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<BoardRunConfig> {
        ostool::board::read_run_config_from_path_for_cargo(&self.invocation, cargo, path).await
    }

    pub(crate) async fn ensure_board_run_config_in_dir_for_cargo(
        &self,
        cargo: &Cargo,
        dir: &Path,
    ) -> anyhow::Result<BoardRunConfig> {
        ostool::board::ensure_run_config_in_dir_for_cargo(&self.invocation, cargo, dir).await
    }

    pub(crate) async fn ensure_uboot_config_for_cargo(
        &self,
        cargo: &Cargo,
    ) -> anyhow::Result<UbootConfig> {
        ostool::run::uboot::ensure_config_for_cargo(&self.invocation, cargo).await
    }

    fn scoped_qemu_path(&self, cargo: &Cargo) -> anyhow::Result<PathRestoreGuard> {
        let guard = PathRestoreGuard::new(self.original_path.clone());
        guard.restore();
        if should_use_loongarch_lvz_for(&cargo.package, &cargo.target) {
            configure_loongarch_qemu_path(&self.root)?;
        }
        Ok(guard)
    }
}

struct EnvRestoreGuard {
    vars: Vec<(OsString, Option<OsString>)>,
}

impl EnvRestoreGuard {
    fn set(vars: &HashMap<String, String>) -> Self {
        let mut saved = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            let key = OsString::from(key);
            let previous = env::var_os(&key);
            // SAFETY: axbuild runs each cargo build/run flow serially in this CLI process.
            // These variables must be visible to ostool's short-lived child cargo probes
            // (for example `cargo tree`) before the final cargo command is constructed.
            unsafe {
                env::set_var(&key, value);
            }
            saved.push((key, previous));
        }
        Self { vars: saved }
    }
}

impl Drop for EnvRestoreGuard {
    fn drop(&mut self) {
        for (key, previous) in self.vars.iter().rev() {
            // SAFETY: see `EnvRestoreGuard::set`; this restores the process environment
            // immediately after the scoped ostool cargo operation completes.
            unsafe {
                match previous {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }
}

struct PathRestoreGuard {
    original_path: OsString,
}

impl PathRestoreGuard {
    fn new(original_path: OsString) -> Self {
        Self { original_path }
    }

    fn restore(&self) {
        unsafe {
            env::set_var("PATH", &self.original_path);
        }
    }
}

impl Drop for PathRestoreGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn should_use_loongarch_lvz_for(package: &str, target: &str) -> bool {
    package == "axvisor" && target.contains("loongarch64")
}

fn configure_loongarch_qemu_path(workspace_root: &Path) -> anyhow::Result<()> {
    let Some(qemu_dir) = find_loongarch_qemu_dir(workspace_root) else {
        return Ok(());
    };

    prepend_dir_to_path(&qemu_dir)?;
    info!(
        "Using LoongArch QEMU from PATH-prepended directory: {}",
        qemu_dir.display()
    );
    Ok(())
}

fn find_loongarch_qemu_dir(workspace_root: &Path) -> Option<PathBuf> {
    let env_executable = env::var_os("AXBUILD_QEMU_SYSTEM_LOONGARCH64")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .and_then(|path| path.parent().map(Path::to_path_buf));
    if let Some(dir) = env_executable.filter(|dir| is_loongarch_qemu_dir(dir)) {
        return Some(dir);
    }

    let env_dir = env::var_os("AXBUILD_QEMU_DIR")
        .map(PathBuf::from)
        .filter(|dir| is_loongarch_qemu_dir(dir));
    if let Some(dir) = env_dir {
        return Some(dir);
    }

    loongarch_qemu_dir_candidates(workspace_root)
        .into_iter()
        .find(|dir| is_loongarch_qemu_dir(dir))
}

fn loongarch_qemu_dir_candidates(workspace_root: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for suffix in ["QEMU-LVZ/build", "qemu-lvz/build"] {
            candidates.push(home.join(suffix));
        }
    }

    for ancestor in workspace_root.ancestors() {
        for suffix in ["QEMU-LVZ/build", "qemu-lvz/build"] {
            candidates.push(ancestor.join(suffix));
        }
    }

    candidates
}

fn is_loongarch_qemu_dir(dir: &Path) -> bool {
    dir.join("qemu-system-loongarch64").exists()
}

fn prepend_dir_to_path(dir: &Path) -> anyhow::Result<()> {
    let current = env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = env::split_paths(&current).collect();
    if paths.iter().any(|path| path == dir) {
        return Ok(());
    }

    paths.insert(0, dir.to_path_buf());
    let joined = env::join_paths(paths.iter())
        .map_err(|err| anyhow::anyhow!("failed to update PATH with {}: {err}", dir.display()))?;

    unsafe {
        env::set_var("PATH", joined);
    }
    Ok(())
}
