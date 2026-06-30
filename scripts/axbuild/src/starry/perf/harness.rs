use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

use super::{
    super::ArgsPerf,
    args_support::{
        effective_callchain, effective_max_depth, host_time_enabled, perf_needs_debuginfo,
        perf_needs_frame_pointers,
    },
    outputs::{PerfOutputs, ensure_file, file_nonempty},
};
use crate::support::process::ProcessExt;

const HARNESS_KIT_REPO: &str = "https://github.com/cg24-THU/tgoskit-harness_kit.git";
const HARNESS_KIT_COMMIT: &str = "762c22725024a065e85b26e0b01121eccea651c0";

pub(super) struct QperfTools {
    pub(super) plugin: PathBuf,
    pub(super) analyzer: PathBuf,
}

pub(super) fn build_qperf_tools(
    root: &Path,
    analyzer_flamegraph: bool,
) -> anyhow::Result<QperfTools> {
    let qperf_root = qperf_source_root(root)?;
    let manifest = qperf_root.join("Cargo.toml");
    let analyzer_manifest = qperf_root.join("analyzer/Cargo.toml");
    let target_dir = qperf_root.join("target");
    if !analyzer_manifest.exists() {
        bail!(
            "qperf analyzer sources not found at {}",
            analyzer_manifest.display()
        );
    }

    Command::new("cargo")
        .current_dir(root)
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .arg("--release")
        .arg("--target-dir")
        .arg(&target_dir)
        .exec()
        .context("failed to build qperf plugin")?;

    let mut analyzer_build = Command::new("cargo");
    analyzer_build
        .current_dir(root)
        .args(["build", "--manifest-path"])
        .arg(&analyzer_manifest)
        .arg("--release")
        .arg("--target-dir")
        .arg(&target_dir);
    if analyzer_flamegraph {
        analyzer_build.args(["--features", "flamegraph"]);
    }
    analyzer_build
        .exec()
        .context("failed to build qperf-analyzer")?;

    let release_dir = target_dir.join("release");
    let plugin_name = if cfg!(target_os = "macos") {
        "libqperf.dylib"
    } else {
        "libqperf.so"
    };
    let tools = QperfTools {
        plugin: release_dir.join(plugin_name),
        analyzer: release_dir.join("qperf-analyzer"),
    };
    ensure_file(&tools.plugin, "qperf plugin")?;
    ensure_file(&tools.analyzer, "qperf analyzer")?;
    Ok(tools)
}

fn qperf_source_root(root: &Path) -> anyhow::Result<PathBuf> {
    if let Some(path) = [root.join("apps/qperf"), root.join("tools/qperf")]
        .into_iter()
        .find(|path| path.join("Cargo.toml").exists())
    {
        return Ok(path);
    }

    let checkout = ensure_harness_kit_checkout(root)?;
    let fixed_qperf = checkout.join("tools/qperf");
    if fixed_qperf.join("Cargo.toml").exists() {
        return Ok(fixed_qperf);
    }

    Err(anyhow::anyhow!(
        "qperf sources not found; expected apps/qperf, tools/qperf, or fixed harness kit \
         tools/qperf to be present"
    ))
}

fn ensure_harness_kit_checkout(root: &Path) -> anyhow::Result<PathBuf> {
    if let Some(checkout) = env::var_os("TGOSKIT_HARNESS_KIT_DIR").map(PathBuf::from) {
        validate_harness_kit_override(&checkout)?;
        return Ok(checkout);
    }

    let checkout = root
        .join("target/tgoskit-harness-kit")
        .join(HARNESS_KIT_COMMIT);
    if checkout.join(".git").is_dir() {
        let actual = git_stdout(Some(&checkout), &["rev-parse", "HEAD"])?;
        if actual == HARNESS_KIT_COMMIT {
            return Ok(checkout);
        }
        git_status(
            Some(&checkout),
            &["fetch", "--depth", "1", "origin", HARNESS_KIT_COMMIT],
        )?;
        git_status(
            Some(&checkout),
            &["checkout", "--detach", HARNESS_KIT_COMMIT],
        )?;
        git_status(Some(&checkout), &["reset", "--hard", HARNESS_KIT_COMMIT])?;
        return Ok(checkout);
    }

    let parent = checkout.parent().ok_or_else(|| {
        anyhow::anyhow!("invalid harness kit checkout path {}", checkout.display())
    })?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let tmp_name = format!(
        "{}.tmp-{}",
        checkout
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("harness-kit"),
        std::process::id()
    );
    let tmp_checkout = checkout.with_file_name(tmp_name);
    if tmp_checkout.exists() {
        fs::remove_dir_all(&tmp_checkout)
            .with_context(|| format!("failed to remove {}", tmp_checkout.display()))?;
    }

    let clone_result = (|| -> anyhow::Result<()> {
        git_status(None, &["init", "-q", path_str(&tmp_checkout)?])?;
        git_status(
            Some(&tmp_checkout),
            &["remote", "add", "origin", HARNESS_KIT_REPO],
        )?;
        git_status(
            Some(&tmp_checkout),
            &["fetch", "--depth", "1", "origin", HARNESS_KIT_COMMIT],
        )?;
        git_status(Some(&tmp_checkout), &["checkout", "--detach", "FETCH_HEAD"])?;
        let actual = git_stdout(Some(&tmp_checkout), &["rev-parse", "HEAD"])?;
        if actual != HARNESS_KIT_COMMIT {
            bail!("fetched harness kit {actual}, expected {HARNESS_KIT_COMMIT}");
        }
        Ok(())
    })();

    if clone_result.is_err() {
        let _ = fs::remove_dir_all(&tmp_checkout);
    }
    clone_result?;
    if checkout.exists() {
        fs::remove_dir_all(&checkout)
            .with_context(|| format!("failed to remove {}", checkout.display()))?;
    }
    fs::rename(&tmp_checkout, &checkout).with_context(|| {
        format!(
            "failed to move {} to {}",
            tmp_checkout.display(),
            checkout.display()
        )
    })?;
    Ok(checkout)
}

fn validate_harness_kit_override(checkout: &Path) -> anyhow::Result<()> {
    ensure_file(
        &checkout.join("tools/qperf/Cargo.toml"),
        "TGOSKIT_HARNESS_KIT_DIR qperf manifest",
    )?;
    ensure_file(
        &checkout.join("tools/qperf/analyzer/Cargo.toml"),
        "TGOSKIT_HARNESS_KIT_DIR qperf analyzer manifest",
    )?;
    ensure_file(
        &checkout.join("tools/starry-syscall-harness/harness.py"),
        "TGOSKIT_HARNESS_KIT_DIR harness script",
    )?;

    if !checkout.join(".git").is_dir() {
        bail!(
            "TGOSKIT_HARNESS_KIT_DIR={} is not a git checkout; cannot verify pinned harness kit \
             commit {}. Use a read-only git checkout at the pinned commit, or unset the variable \
             to let xtask manage target/tgoskit-harness-kit",
            checkout.display(),
            HARNESS_KIT_COMMIT
        );
    }

    let actual = git_stdout(Some(checkout), &["rev-parse", "HEAD"])?;
    if actual != HARNESS_KIT_COMMIT {
        bail!(
            "TGOSKIT_HARNESS_KIT_DIR={} is at commit {}, expected {}; the override path is \
             read-only and will not be fetched, reset, or replaced",
            checkout.display(),
            actual,
            HARNESS_KIT_COMMIT
        );
    }
    Ok(())
}

fn git_status(cwd: Option<&Path>, args: &[&str]) -> anyhow::Result<()> {
    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.arg("-C").arg(cwd);
    }
    let status = command
        .args(args)
        .status()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !status.success() {
        bail!("git {} failed with {status}", args.join(" "));
    }
    Ok(())
}

fn git_stdout(cwd: Option<&Path>, args: &[&str]) -> anyhow::Result<String> {
    let mut command = Command::new("git");
    if let Some(cwd) = cwd {
        command.arg("-C").arg(cwd);
    }
    let output = command
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("git {} failed with {}", args.join(" "), output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn path_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

pub(super) fn run_report_postprocess(
    root: &Path,
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    arch: &str,
    returncode: i32,
) -> anyhow::Result<PathBuf> {
    let harness =
        match workspace_harness_path(&outputs.work_dir).or_else(|| workspace_harness_path(root)) {
            Some(harness) => harness,
            None => {
                let checkout = ensure_harness_kit_checkout(root)?;
                let harness = checkout.join("tools/starry-syscall-harness/harness.py");
                ensure_file(&harness, "fixed harness kit postprocess script")?;
                harness
            }
        };
    let python = env::var_os("STARRY_SYSCALL_HARNESS_PYTHON")
        .or_else(|| env::var_os("PYTHON"))
        .unwrap_or_else(|| OsString::from("python3"));
    let mut command = Command::new(python);
    command
        .arg(&harness)
        .arg("perf-postprocess")
        .arg("--repo-root")
        .arg(root)
        .arg("--arch")
        .arg(arch)
        .arg("--work-dir")
        .arg(&outputs.work_dir)
        .arg("--qperf-dir")
        .arg(&outputs.dir)
        .arg("--returncode")
        .arg(returncode.to_string())
        .arg("--timeout")
        .arg(args.timeout.to_string())
        .arg("--format")
        .arg(format!("{:?}", args.format).to_ascii_lowercase())
        .arg("--freq")
        .arg(args.freq.to_string())
        .arg("--max-depth")
        .arg(effective_max_depth(args).to_string())
        .arg("--mode")
        .arg(args.mode.to_string())
        .arg("--callchain")
        .arg(effective_callchain(args).to_string())
        .arg("--top")
        .arg(args.top.to_string())
        .arg("--min-percent")
        .arg(args.min_percent.to_string())
        .arg("--symbol-style")
        .arg(args.symbol_style.to_string())
        .arg("--profile-stdout")
        .arg(&outputs.profile_stdout)
        .arg("--profile-stderr")
        .arg(&outputs.profile_stderr);
    if args.debug {
        command.arg("--debug");
    }
    if args.kernel_filter {
        command.arg("--kernel-filter");
    }
    if host_time_enabled(args) {
        command.arg("--host-time");
    }
    if args.host_perf {
        command
            .arg("--host-perf")
            .arg("--host-perf-events")
            .arg(&args.host_perf_events);
    }
    if let Some(cmd) = &args.shell_init_cmd {
        command.arg("--shell-init-cmd").arg(cmd);
    }
    if let Some(prefix) = &args.shell_prefix {
        command.arg("--shell-prefix").arg(prefix);
    }
    if let Some(marker) = &args.start_marker {
        command.arg("--start-marker").arg(marker);
    }
    if let Some(marker) = &args.stop_marker {
        command.arg("--stop-marker").arg(marker);
    }
    if let Some(timeout) = args.workload_timeout {
        command.arg("--workload-timeout").arg(timeout.to_string());
    }
    if args.qperf_metrics {
        command.arg("--qperf-metrics");
    }
    if args.full_stack {
        command.arg("--full-stack");
    }
    if perf_needs_debuginfo(args) {
        command.arg("--perf-debuginfo");
    }
    if perf_needs_frame_pointers(args) {
        command.arg("--perf-force-frame-pointers");
    }
    if let Some(focus) = &args.focus {
        command.arg("--focus").arg(focus);
    }
    if args.no_truncate {
        command.arg("--no-truncate");
    }
    for qemu_arg in &args.qemu_args {
        command.arg("--qemu-arg").arg(qemu_arg);
    }
    let status = command
        .status()
        .context("failed to run qperf report postprocess")?;
    if !status.success() {
        bail!("qperf report postprocess failed with {status}");
    }
    ensure_report_outputs(outputs)?;
    Ok(harness)
}

fn ensure_report_outputs(outputs: &PerfOutputs) -> anyhow::Result<()> {
    for path in [
        &outputs.report_json,
        &outputs.report_md,
        &outputs.hotspots_csv,
        &outputs.hotspot_categories_csv,
    ] {
        if !file_nonempty(path) {
            bail!(
                "qperf report postprocess did not generate expected artifact: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn workspace_harness_path(work_dir: &Path) -> Option<PathBuf> {
    let mut current = Some(work_dir);
    while let Some(path) = current {
        for candidate in [
            path.join("apps/OScope-harness/harness.py"),
            path.join("tools/starry-syscall-harness/harness.py"),
        ] {
            if candidate.exists() {
                return Some(candidate);
            }
        }
        current = path.parent();
    }
    None
}
