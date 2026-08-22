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

const LEGACY_PERF_POSTPROCESS_ARCH_ARG: &str = r#"perf_post_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64"])"#;
const X86_64_PERF_POSTPROCESS_ARCH_ARG: &str = r#"perf_post_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64", "x86_64"])"#;
const HARNESS_KIT_PREBUILD: &str = "apps/common/prebuild-harness-kit.sh";

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
    let prebuild = root.join(HARNESS_KIT_PREBUILD);
    ensure_file(&prebuild, "shared harness kit prebuild provider")?;
    let output = Command::new(&prebuild)
        .env("STARRY_WORKSPACE", root)
        .output()
        .with_context(|| format!("failed to run {}", prebuild.display()))?;
    if !output.status.success() {
        bail!(
            "shared harness kit prebuild provider failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    checkout_path_from_stdout(output.stdout)
}

fn checkout_path_from_stdout(mut stdout: Vec<u8>) -> anyhow::Result<PathBuf> {
    if stdout.last() == Some(&b'\n') {
        stdout.pop();
        if stdout.last() == Some(&b'\r') {
            stdout.pop();
        }
    }
    if stdout.is_empty() {
        bail!("shared harness kit prebuild provider returned an empty path");
    }
    if stdout.contains(&b'\n') || stdout.contains(&b'\r') || stdout.contains(&b'\0') {
        bail!("shared harness kit prebuild provider returned an invalid path");
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        Ok(PathBuf::from(OsString::from_vec(stdout)))
    }
    #[cfg(not(unix))]
    {
        let checkout = String::from_utf8(stdout)
            .context("shared harness kit prebuild provider returned a non-UTF-8 path")?;
        Ok(PathBuf::from(checkout))
    }
}

pub(super) fn run_report_postprocess(
    root: &Path,
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    arch: &str,
    returncode: i32,
) -> anyhow::Result<PathBuf> {
    let harness = report_harness(root, outputs, arch)?;
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
        command.arg("--host-perf");
        command.arg_option_value("--host-perf-events", args.host_perf_events.as_ref());
    }
    if let Some(cmd) = &args.shell_init_cmd {
        command.arg_option_value("--shell-init-cmd", cmd.as_ref());
    }
    if let Some(prefix) = &args.shell_prefix {
        command.arg_option_value("--shell-prefix", prefix.as_ref());
    }
    if let Some(marker) = &args.start_marker {
        command.arg_option_value("--start-marker", marker.as_ref());
    }
    if let Some(marker) = &args.stop_marker {
        command.arg_option_value("--stop-marker", marker.as_ref());
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
        command.arg_option_value("--focus", focus.as_ref());
    }
    if args.no_truncate {
        command.arg("--no-truncate");
    }
    append_qemu_args(&mut command, &args.qemu_args);
    let status = command
        .status()
        .context("failed to run qperf report postprocess")?;
    if !status.success() {
        bail!("qperf report postprocess failed with {status}");
    }
    ensure_report_outputs(outputs)?;
    Ok(harness)
}

fn append_qemu_args(command: &mut Command, qemu_args: &[String]) {
    for qemu_arg in qemu_args {
        command.arg_option_value("--qemu-arg", qemu_arg.as_ref());
    }
}

fn report_harness(root: &Path, outputs: &PerfOutputs, arch: &str) -> anyhow::Result<PathBuf> {
    if arch == "x86_64" {
        let checkout = ensure_harness_kit_checkout(root)?;
        let upstream = checkout.join("tools/starry-syscall-harness/harness.py");
        ensure_file(&upstream, "fixed harness kit postprocess script")?;
        let source = fs::read_to_string(&upstream)
            .with_context(|| format!("failed to read {}", upstream.display()))?;
        let patched = add_x86_64_perf_postprocess_choice(&source)?;
        let harness = outputs.dir.join("harness-x86_64.py");
        fs::write(&harness, patched)
            .with_context(|| format!("failed to write {}", harness.display()))?;
        return Ok(harness);
    }

    match workspace_harness_path(&outputs.work_dir).or_else(|| workspace_harness_path(root)) {
        Some(harness) => Ok(harness),
        None => {
            let checkout = ensure_harness_kit_checkout(root)?;
            let harness = checkout.join("tools/starry-syscall-harness/harness.py");
            ensure_file(&harness, "fixed harness kit postprocess script")?;
            Ok(harness)
        }
    }
}

fn add_x86_64_perf_postprocess_choice(source: &str) -> anyhow::Result<String> {
    let legacy_matches = source.matches(LEGACY_PERF_POSTPROCESS_ARCH_ARG).count();
    if legacy_matches != 1 {
        bail!(
            "cannot apply the x86_64 qperf postprocess compatibility patch: expected exactly one \
             pinned legacy perf-postprocess architecture declaration, found {legacy_matches}"
        );
    }
    Ok(source.replacen(
        LEGACY_PERF_POSTPROCESS_ARCH_ARG,
        X86_64_PERF_POSTPROCESS_ARCH_ARG,
        1,
    ))
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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{add_x86_64_perf_postprocess_choice, append_qemu_args, checkout_path_from_stdout};

    #[test]
    fn x86_64_postprocess_shim_extends_only_the_perf_postprocess_arch_choice() {
        let legacy = r#"perf_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64"])
perf_post_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64"])"#;

        let patched = add_x86_64_perf_postprocess_choice(legacy).unwrap();

        assert!(patched.contains(
            r#"perf_post_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64", "x86_64"])"#
        ));
        assert!(patched.contains(
            r#"perf_parser.add_argument("--arch", default="riscv64", choices=["riscv64", "loongarch64"])"#
        ));
    }

    #[test]
    fn postprocess_qemu_args_encode_hyphen_prefixed_values_as_one_argument() {
        let mut command = Command::new("python3");

        append_qemu_args(&mut command, &["-cpu".into(), "max".into()]);

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args, ["--qemu-arg=-cpu", "--qemu-arg=max"]);
    }

    #[test]
    fn checkout_provider_path_preserves_trailing_spaces() {
        let checkout = checkout_path_from_stdout(b"/tmp/harness kit \n".to_vec()).unwrap();

        assert_eq!(checkout, std::path::Path::new("/tmp/harness kit "));
    }

    #[test]
    fn checkout_provider_path_rejects_multiple_output_lines() {
        let error = checkout_path_from_stdout(b"notice\n/tmp/harness\n".to_vec()).unwrap_err();

        assert!(error.to_string().contains("invalid path"));
    }

    #[cfg(unix)]
    #[test]
    fn checkout_provider_path_preserves_non_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let checkout = checkout_path_from_stdout(b"/tmp/harness-\xff\n".to_vec()).unwrap();

        assert_eq!(checkout.as_os_str().as_bytes(), b"/tmp/harness-\xff");
    }
}
