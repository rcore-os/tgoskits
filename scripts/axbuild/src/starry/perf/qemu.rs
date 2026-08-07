use std::{
    fs,
    fs::File,
    path::Path,
    process::{Command, ExitStatus},
    time::Instant,
};

use anyhow::{Context, bail};
use ostool::ovmf::Arch;
use serde::{Deserialize, Serialize};

use super::{
    super::ArgsPerf,
    args_support::{effective_callchain, effective_max_depth, host_time_enabled},
    harness::QperfTools,
    metrics::{child_resource_usage, write_host_perf_unavailable, write_host_time_metrics},
    monitor::{
        PerfWindowReport, run_qemu_with_stdout_monitor, window_report_from_config,
        write_window_report,
    },
    outputs::{PerfOutputs, ensure_file},
    symbols::KernelTextRange,
    toolchain::find_executable,
};

pub(super) const QPERF_QUEUE_SIZE: usize = 4096;
pub(super) const DEFAULT_STARRY_SHELL_PREFIX: &str = "root@starry:";
const SUPPORTED_ARCHES: &str = "riscv64, loongarch64, and x86_64";

#[derive(Deserialize, Serialize)]
pub(super) struct PerfQemuConfig {
    pub(super) args: Vec<String>,
    pub(super) uefi: bool,
    pub(super) to_bin: bool,
    pub(super) success_regex: Vec<String>,
    pub(super) fail_regex: Vec<String>,
    pub(super) shell_prefix: Option<String>,
    pub(super) shell_init_cmd: Option<String>,
    pub(super) timeout: Option<u64>,
    pub(super) start_marker: Option<String>,
    pub(super) stop_marker: Option<String>,
    pub(super) workload_timeout: Option<u64>,
}

pub(super) struct QemuRun {
    pub(super) status: ExitStatus,
    pub(super) timed_out: bool,
    pub(super) window: PerfWindowReport,
}

pub(super) fn write_qemu_config(
    outputs: &PerfOutputs,
    tools: &QperfTools,
    args: &ArgsPerf,
    arch: &str,
    qemu: &ostool::run::qemu::QemuConfig,
    text_range: Option<KernelTextRange>,
) -> anyhow::Result<()> {
    let mut perf_qemu_args = vec!["-plugin".to_string()];
    let mut plugin_params = format!(
        "{},freq={},max_depth={},queue_size={},mode={},callchain={},out={}",
        tools.plugin.display(),
        args.freq,
        effective_max_depth(args),
        QPERF_QUEUE_SIZE,
        args.mode,
        effective_callchain(args),
        outputs.raw.display()
    );
    plugin_params.push_str(&format!(
        ",filter_kernel={}",
        if args.kernel_filter { 1 } else { 0 }
    ));
    append_text_filter_params(&mut plugin_params, arch, text_range);
    perf_qemu_args.push(plugin_params);
    let mut qemu_args = direct_qemu_args(arch, qemu.args.clone())?;
    qemu_args.extend(args.qemu_args.iter().cloned());
    if qemu_stdout_monitor_enabled(args) && !has_qemu_option(&qemu_args, "-qmp") {
        qemu_args.extend([
            "-qmp".to_string(),
            format!("unix:{},server=on,wait=off", outputs.qmp_socket.display()),
        ]);
    }
    perf_qemu_args.extend(qemu_args);

    let shell_init_cmd = args
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
        .map(str::to_string);
    let shell_prefix = shell_init_cmd.as_ref().map(|_| {
        args.shell_prefix
            .clone()
            .unwrap_or_else(|| DEFAULT_STARRY_SHELL_PREFIX.to_string())
    });

    let config = PerfQemuConfig {
        args: perf_qemu_args,
        uefi: qemu.uefi,
        to_bin: qemu.to_bin,
        success_regex: Vec::new(),
        fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
        shell_prefix,
        shell_init_cmd,
        timeout: (args.timeout > 0).then_some(args.timeout),
        start_marker: args.start_marker.clone(),
        stop_marker: args.stop_marker.clone(),
        workload_timeout: args.workload_timeout,
    };
    fs::write(&outputs.qemu_config, toml::to_string_pretty(&config)?)
        .with_context(|| format!("failed to write {}", outputs.qemu_config.display()))?;
    Ok(())
}

fn append_text_filter_params(
    plugin_params: &mut String,
    arch: &str,
    text_range: Option<KernelTextRange>,
) {
    let Some(range) = text_range else {
        return;
    };
    let start = range.virt.start;
    let end = range.virt.end;
    plugin_params.push_str(&format!(",filter_start=0x{start:x},filter_end=0x{end:x}"));
    // x86_64 executes unrelated firmware and identity-mapped code in the same low 32-bit
    // address window during UEFI boot. A low alias inferred only by masking the ELF virtual
    // address would therefore turn those samples into plausible but incorrect kernel symbols.
    if arch != "x86_64"
        && let Some(phys) = range.phys
    {
        let offset = range.virt.start.wrapping_sub(phys.start);
        plugin_params.push_str(&format!(
            ",filter_alias_start=0x{:x},filter_alias_end=0x{:x},filter_alias_offset=0x{:x}",
            phys.start, phys.end, offset
        ));
    }
}

pub(super) fn validate_arch(arch: &str) -> anyhow::Result<()> {
    match arch {
        "riscv64" | "loongarch64" | "x86_64" => Ok(()),
        _ => bail!("qperf currently supports StarryOS {SUPPORTED_ARCHES} only"),
    }
}

fn direct_qemu_args(arch: &str, mut args: Vec<String>) -> anyhow::Result<Vec<String>> {
    match arch {
        "riscv64" | "loongarch64" => {
            if !has_qemu_option(&args, "-machine") {
                args.splice(0..0, ["-machine".to_string(), "virt".to_string()]);
            }
        }
        "x86_64" => {}
        _ => bail!("qperf currently supports StarryOS {SUPPORTED_ARCHES} only"),
    }
    Ok(args)
}

fn has_qemu_option(args: &[String], option: &str) -> bool {
    args.iter().any(|arg| arg == option)
}

pub(super) async fn run_qemu_direct(
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    arch: &str,
    kernel_bin: &Path,
) -> anyhow::Result<QemuRun> {
    ensure_file(kernel_bin, "StarryOS kernel image")?;
    let qemu = qemu_executable(arch)?;
    let config = qemu_config_from_path(&outputs.qemu_config)?;
    let qemu_args = config.args.clone();
    let monitor_stdout = qemu_stdout_monitor_enabled(args);

    let mut command_args = qemu_command_prefix(qemu, args.timeout, monitor_stdout);
    command_args.extend(qemu_args);
    command_args.extend(prepare_boot_args(outputs, &config, arch, kernel_bin).await?);

    if args.host_perf {
        if let Some(perf) = find_executable("perf") {
            let mut wrapped = vec![
                perf.display().to_string(),
                "stat".to_string(),
                "-x".to_string(),
                ",".to_string(),
                "-o".to_string(),
                outputs.host_perf.display().to_string(),
                "-e".to_string(),
                args.host_perf_events.clone(),
                "--".to_string(),
            ];
            wrapped.extend(command_args);
            command_args = wrapped;
        } else {
            write_host_perf_unavailable(&outputs.host_perf, "perf not found in PATH")?;
            eprintln!("qperf: --host-perf requested but `perf` was not found in PATH");
        }
    }

    let mut command = Command::new(&command_args[0]);
    command.args(&command_args[1..]);
    eprintln!("running qperf QEMU: {command:?}");
    let host_wall_start = Instant::now();
    let host_usage_start = child_resource_usage();
    let qemu_run = if monitor_stdout {
        run_qemu_with_stdout_monitor(command, &config, outputs, args.timeout)?
    } else {
        let status = command.status().context("failed to spawn QEMU")?;
        QemuRun {
            timed_out: args.timeout > 0 && status.code() == Some(124),
            status,
            window: window_report_from_config(&config),
        }
    };
    if host_time_enabled(args) {
        write_host_time_metrics(
            &outputs.host_time,
            host_wall_start.elapsed(),
            host_usage_start,
            child_resource_usage(),
            &qemu_run.status,
        )?;
    }
    write_window_report(&outputs.window, &qemu_run.window)?;
    if !outputs.profile_stdout.exists() {
        File::create(&outputs.profile_stdout)
            .with_context(|| format!("failed to create {}", outputs.profile_stdout.display()))?;
    }
    if !outputs.profile_stderr.exists() {
        File::create(&outputs.profile_stderr)
            .with_context(|| format!("failed to create {}", outputs.profile_stderr.display()))?;
    }
    Ok(qemu_run)
}

fn qemu_command_prefix(qemu: &str, timeout: u64, monitor_stdout: bool) -> Vec<String> {
    if timeout > 0 && !monitor_stdout {
        vec![
            "timeout".to_string(),
            "--foreground".to_string(),
            "--signal=INT".to_string(),
            "--kill-after=5s".to_string(),
            format!("{timeout}s"),
            qemu.to_string(),
        ]
    } else {
        vec![qemu.to_string()]
    }
}

async fn prepare_boot_args(
    outputs: &PerfOutputs,
    config: &PerfQemuConfig,
    arch: &str,
    kernel_bin: &Path,
) -> anyhow::Result<Vec<String>> {
    if arch != "x86_64" {
        return Ok(vec![
            "-kernel".to_string(),
            kernel_bin.display().to_string(),
        ]);
    }
    if !config.uefi {
        bail!("StarryOS x86_64 qperf requires a QEMU config with `uefi = true`");
    }
    if !config.to_bin {
        bail!("StarryOS x86_64 qperf requires a QEMU config with `to_bin = true`");
    }

    let firmware = crate::support::ovmf::OvmfFirmware::fetch(Arch::X64).await?;
    prepare_x86_64_uefi_boot(&outputs.dir, kernel_bin, &firmware)
}

fn prepare_x86_64_uefi_boot(
    output_dir: &Path,
    kernel_bin: &Path,
    firmware: &crate::support::ovmf::OvmfFirmware,
) -> anyhow::Result<Vec<String>> {
    ensure_file(kernel_bin, "StarryOS x86_64 UEFI image")?;
    ensure_file(firmware.code(), "OVMF code image")?;
    ensure_file(firmware.vars(), "OVMF vars template")?;

    let esp_dir = output_dir.join("starryos.esp");
    let boot_dir = esp_dir.join("EFI/BOOT");
    fs::create_dir_all(&boot_dir)
        .with_context(|| format!("failed to create x86_64 UEFI ESP {}", boot_dir.display()))?;
    let boot_image = boot_dir.join("BOOTX64.EFI");
    fs::copy(kernel_bin, &boot_image).with_context(|| {
        format!(
            "failed to copy StarryOS UEFI image from {} to {}",
            kernel_bin.display(),
            boot_image.display()
        )
    })?;

    let vars = output_dir.join("starryos.vars.fd");
    fs::copy(firmware.vars(), &vars).with_context(|| {
        format!(
            "failed to copy OVMF vars template from {} to {}",
            firmware.vars().display(),
            vars.display()
        )
    })?;

    Ok(vec![
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            firmware.code().display()
        ),
        "-drive".to_string(),
        format!("if=pflash,format=raw,unit=1,file={}", vars.display()),
        "-drive".to_string(),
        format!("format=raw,file=fat:rw:{}", esp_dir.display()),
    ])
}

fn qemu_executable(arch: &str) -> anyhow::Result<&'static str> {
    let name = match arch {
        "riscv64" => "qemu-system-riscv64",
        "loongarch64" => "qemu-system-loongarch64",
        "x86_64" => "qemu-system-x86_64",
        _ => bail!("qperf currently supports StarryOS {SUPPORTED_ARCHES} only"),
    };
    if find_executable(name).is_none() {
        bail!(
            "qperf requires `{name}` in PATH; install the matching QEMU system emulator or run \
             the Docker-based harness perf-profile entrypoint"
        );
    }
    Ok(name)
}

fn qemu_config_from_path(path: &Path) -> anyhow::Result<PerfQemuConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read qperf QEMU config {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse qperf QEMU config {}", path.display()))
}

fn qemu_stdout_monitor_enabled(args: &ArgsPerf) -> bool {
    args.shell_init_cmd
        .as_deref()
        .is_some_and(|cmd| !cmd.trim().is_empty())
        || args.start_marker.is_some()
        || args.stop_marker.is_some()
        || args.workload_timeout.is_some()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        append_text_filter_params, direct_qemu_args, prepare_x86_64_uefi_boot, qemu_command_prefix,
        validate_arch,
    };
    use crate::{
        starry::perf::symbols::{AddressRange, KernelTextRange},
        support::ovmf::OvmfFirmware,
    };

    #[test]
    fn direct_qemu_args_accepts_x86_64_q35_config() {
        let args = vec!["-machine".to_string(), "q35".to_string()];

        let args = direct_qemu_args("x86_64", args.clone()).unwrap();

        assert_eq!(args, vec!["-machine", "q35"]);
    }

    #[test]
    fn supported_arch_validation_includes_x86_64() {
        assert!(validate_arch("x86_64").is_ok());
        assert!(validate_arch("aarch64").is_err());
    }

    #[test]
    fn timeout_keeps_interactive_qemu_in_the_foreground() {
        let prefix = qemu_command_prefix("qemu-system-x86_64", 15, false);

        assert_eq!(
            prefix,
            vec![
                "timeout",
                "--foreground",
                "--signal=INT",
                "--kill-after=5s",
                "15s",
                "qemu-system-x86_64",
            ]
        );
    }

    #[test]
    fn x86_64_kernel_filter_does_not_guess_a_low_address_alias() {
        let mut params = String::new();
        append_text_filter_params(
            &mut params,
            "x86_64",
            Some(KernelTextRange {
                virt: AddressRange {
                    start: 0xffff_ffff_8000_0000,
                    end: 0xffff_ffff_804d_383f,
                },
                phys: Some(AddressRange {
                    start: 0x8000_0000,
                    end: 0x804d_383f,
                }),
            }),
        );

        assert!(params.contains("filter_start=0xffffffff80000000"));
        assert!(!params.contains("filter_alias_start"));
    }

    #[test]
    fn x86_64_uefi_boot_uses_a_private_vars_copy_and_esp() {
        let temp = tempfile::tempdir().unwrap();
        let kernel_bin = temp.path().join("starryos.bin");
        let code = temp.path().join("code.fd");
        let vars = temp.path().join("vars.fd");
        fs::write(&kernel_bin, b"MZ kernel").unwrap();
        fs::write(&code, b"code").unwrap();
        fs::write(&vars, b"vars").unwrap();

        let args = prepare_x86_64_uefi_boot(
            temp.path(),
            &kernel_bin,
            &OvmfFirmware::from_paths(code.clone(), vars),
        )
        .unwrap();

        assert_eq!(
            fs::read(temp.path().join("starryos.esp/EFI/BOOT/BOOTX64.EFI")).unwrap(),
            b"MZ kernel"
        );
        assert_eq!(
            fs::read(temp.path().join("starryos.vars.fd")).unwrap(),
            b"vars"
        );
        assert!(args.iter().any(|arg| arg.contains(code.to_str().unwrap())));
        assert!(!args.iter().any(|arg| arg == "-kernel"));
    }
}
