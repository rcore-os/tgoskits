use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use log::info;
use ostool::{
    board::{self as ostool_board, RunBoardOptions, config::BoardRunConfig},
    build::{
        self as ostool_build, CargoQemuRunnerArgs, CargoRunnerKind, CargoUbootRunnerArgs,
        RuntimeArtifactInput,
        config::{BuildConfig, BuildSystem, Cargo},
    },
    invocation::{Invocation, InvocationOptions},
    run::{
        qemu::{self as ostool_qemu, QemuConfig, RunQemuOptions},
        uboot::{self as ostool_uboot, UbootConfig},
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
    resolve_starry_arch_and_target, starry_arch_for_target_checked, starry_target_for_arch_checked,
    supported_arches, supported_targets, validate_supported_target,
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

        let invocation = Self::new_invocation(&workspace_root, false)
            .context("failed to initialize ostool invocation")?;
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
    ) -> anyhow::Result<ostool_build::CargoBuildOutput> {
        self.set_build_config_path(build_config_path);
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        let build_config_path = self.build_config_path.clone();
        let stage = StageLog::start(format!(
            "cargo build package={} target={} config={}",
            cargo.package,
            cargo.target,
            display_optional_path(build_config_path.as_deref())
        ));
        let output =
            ostool_build::cargo_build(&mut self.invocation, &cargo, build_config_path.as_deref())
                .await?;
        stage.done();
        println!("[axbuild] cargo build elf={}", output.elf_path().display());
        println!(
            "[axbuild] cargo build artifact_dir={}",
            output.cargo_artifact_dir().display()
        );
        Ok(output)
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
        qemu: Option<QemuConfig>,
    ) -> anyhow::Result<()> {
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        let _path_guard = self.scoped_qemu_path(&cargo)?;
        self.set_build_config_path(build_config_path);
        let build_config_path = self.build_config_path.clone();
        let stage = StageLog::start(format!(
            "qemu build+run package={} target={} config={}",
            cargo.package,
            cargo.target,
            display_optional_path(build_config_path.as_deref())
        ));
        let result = ostool_build::cargo_run(
            &mut self.invocation,
            &cargo,
            build_config_path.as_deref(),
            &CargoRunnerKind::Qemu(Box::new(CargoQemuRunnerArgs {
                qemu,
                debug: self.debug,
                dtb_dump: false,
            })),
        )
        .await;
        if result.is_ok() {
            stage.done();
        }
        result
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
        self.activate_cargo_build_context(cargo)?;
        let stage = StageLog::start(format!(
            "qemu run package={} target={}",
            cargo.package, cargo.target
        ));
        let result = ostool_qemu::run_qemu(
            &mut self.invocation,
            &qemu,
            RunQemuOptions { dtb_dump: false },
        )
        .await;
        if result.is_ok() {
            stage.done();
        }
        result
    }

    pub(crate) async fn run_qemu_with_axtest_coverage(
        &mut self,
        cargo: &Cargo,
        mut qemu: QemuConfig,
        capture_backtrace: Option<crate::backtrace::BacktraceQemuCapture>,
    ) -> anyhow::Result<()> {
        if !crate::support::axtest_coverage::enabled(cargo) {
            return self.run_qemu(cargo, qemu, capture_backtrace).await;
        }

        let paths = crate::support::axtest_coverage::AxtestCoveragePaths::new(
            self.workspace_root(),
            &cargo.package,
            &cargo.target,
        )?;
        crate::support::axtest_coverage::apply_qemu_monitor(&mut qemu, &paths)?;
        crate::support::axtest_coverage::update_success_regex(&mut qemu);
        let capture = crate::support::axtest_coverage::AxtestCoverageCaptureGuard::install(&paths)
            .context("failed to install axtest coverage capture")?;
        let result = self.run_qemu(cargo, qemu, capture_backtrace).await;
        capture.finish()?;
        result
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
        let stage = StageLog::start("qemu run prepared artifact");
        let result = ostool_qemu::run_qemu(
            &mut self.invocation,
            &qemu,
            RunQemuOptions { dtb_dump: false },
        )
        .await;
        if result.is_ok() {
            stage.done();
        }
        result
    }

    pub(crate) async fn run_prepared_uboot(&mut self, uboot: UbootConfig) -> anyhow::Result<()> {
        let stage = StageLog::start("uboot run prepared artifact");
        let result = ostool_uboot::run_uboot(&mut self.invocation, &uboot).await;
        if result.is_ok() {
            stage.done();
        }
        result
    }

    pub(crate) async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot: Option<UbootConfig>,
    ) -> anyhow::Result<()> {
        let _env_guard = EnvRestoreGuard::set(&cargo.env);
        self.set_build_config_path(build_config_path);
        let build_config_path = self.build_config_path.clone();
        let stage = StageLog::start(format!(
            "uboot build+run package={} target={} config={}",
            cargo.package,
            cargo.target,
            display_optional_path(build_config_path.as_deref())
        ));
        let result = ostool_build::cargo_run(
            &mut self.invocation,
            &cargo,
            build_config_path.as_deref(),
            &CargoRunnerKind::Uboot(Box::new(CargoUbootRunnerArgs { uboot })),
        )
        .await;
        if result.is_ok() {
            stage.done();
        }
        result
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
        let build_config_path = self.build_config_path.clone();
        let stage = StageLog::start(format!(
            "board build+run package={} target={} config={}",
            cargo.package,
            cargo.target,
            display_optional_path(build_config_path.as_deref())
        ));
        let result = ostool_board::cargo_run_board(
            &mut self.invocation,
            &cargo,
            build_config_path.as_deref(),
            &board_config,
            options,
        )
        .await;
        if result.is_ok() {
            stage.done();
        }
        result
    }

    pub(crate) async fn board_prepared_elf(
        &mut self,
        elf_path: PathBuf,
        to_bin: bool,
        build_config_path: PathBuf,
        board_config: BoardRunConfig,
        options: RunBoardOptions,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        let prepare_stage = StageLog::start(format!(
            "prepare runtime artifact elf={} to_bin={}",
            elf_path.display(),
            to_bin
        ));
        ostool_build::prepare_runtime_artifact(
            &mut self.invocation,
            RuntimeArtifactInput::new(elf_path, to_bin),
        )?;
        prepare_stage.done();

        let run_stage = StageLog::start("board run prepared artifact");
        let result =
            ostool_board::run_prepared_board(&mut self.invocation, &board_config, options).await;
        if result.is_ok() {
            run_stage.done();
        }
        result
    }

    pub(crate) fn set_debug_mode(&mut self, debug: bool) -> anyhow::Result<()> {
        if self.debug == debug {
            return Ok(());
        }

        self.invocation = Self::new_invocation(&self.root, debug)
            .context("failed to reinitialize ostool invocation")?;
        self.debug = debug;

        Ok(())
    }

    pub(crate) async fn read_qemu_config_from_path_for_cargo(
        &self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<QemuConfig> {
        ostool_qemu::read_config_from_path_for_cargo(&self.invocation, cargo, path).await
    }

    pub(crate) async fn read_uboot_config_from_path_for_cargo(
        &self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<UbootConfig> {
        ostool_uboot::read_config_from_path_for_cargo(&self.invocation, cargo, path).await
    }

    pub(crate) async fn ensure_uboot_config_for_cargo(
        &self,
        cargo: &Cargo,
    ) -> anyhow::Result<UbootConfig> {
        ostool_uboot::ensure_config_for_cargo(&self.invocation, cargo).await
    }

    pub(crate) async fn read_board_run_config_from_path_for_cargo(
        &self,
        cargo: &Cargo,
        path: &Path,
    ) -> anyhow::Result<BoardRunConfig> {
        ostool_board::read_run_config_from_path_for_cargo(&self.invocation, cargo, path).await
    }

    pub(crate) async fn ensure_board_run_config_in_dir_for_cargo(
        &self,
        cargo: &Cargo,
        dir: &Path,
    ) -> anyhow::Result<BoardRunConfig> {
        ostool_board::ensure_run_config_in_dir_for_cargo(&self.invocation, cargo, dir).await
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path);
    }

    fn activate_cargo_build_context(&mut self, cargo: &Cargo) -> anyhow::Result<()> {
        let build_config_path = self.build_config_path.clone();
        ostool_build::activate_build_config(
            &mut self.invocation,
            &BuildConfig {
                system: BuildSystem::Cargo(Box::new(cargo.clone())),
            },
            build_config_path.as_deref(),
        )
    }

    fn new_invocation(workspace_root: &Path, debug: bool) -> anyhow::Result<Invocation> {
        Invocation::new(InvocationOptions::new(
            Some(workspace_root.join("Cargo.toml")),
            None,
            None,
            debug,
        ))
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

struct StageLog {
    name: String,
    started: Instant,
}

impl StageLog {
    fn start(name: impl Into<String>) -> Self {
        let name = name.into();
        println!("[axbuild] {name} ...");
        Self {
            name,
            started: Instant::now(),
        }
    }

    fn done(self) {
        println!(
            "[axbuild] {} ... done ({:.2?})",
            self.name,
            self.started.elapsed()
        );
    }
}

fn display_optional_path(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_else(|| "<default>".to_string())
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

    let cache_root = env::var_os("AXVISOR_QEMU_LVZ_CACHE")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/axvisor/qemu-lvz"))
        });
    if let Some(cache_root) = cache_root {
        candidates.push(cache_root.join("latest").join("bin"));
        if let Some(commit) = pinned_qemu_lvz_commit(workspace_root) {
            candidates.push(cache_root.join(commit).join("bin"));
        }
        candidates.extend(cached_loongarch_qemu_dirs(&cache_root));
    }

    candidates
}

fn cached_loongarch_qemu_dirs(cache_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache_root) else {
        return Vec::new();
    };

    let mut dirs: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "src") {
                return None;
            }
            let bin_dir = path.join("bin");
            is_loongarch_qemu_dir(&bin_dir).then_some(bin_dir)
        })
        .collect();
    dirs.sort();
    dirs
}

fn pinned_qemu_lvz_commit(workspace_root: &Path) -> Option<String> {
    let version_file = workspace_root.join("os/axvisor/scripts/qemu-lvz.version");
    let content = std::fs::read_to_string(version_file).ok()?;
    content
        .lines()
        .find_map(|line| line.strip_prefix("QEMU_LVZ_COMMIT="))
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
        .map(str::to_owned)
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
