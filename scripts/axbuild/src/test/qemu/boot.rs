use super::*;

pub(super) struct QemuArgs<'a> {
    args: &'a [String],
}

impl<'a> QemuArgs<'a> {
    fn new(args: &'a [String]) -> Self {
        Self { args }
    }

    fn option_value(&self, option: &str) -> Option<&str> {
        let index = self.args.iter().position(|arg| arg == option)?;
        self.args.get(index + 1).map(String::as_str)
    }
}

pub(super) struct QemuArgsMut<'a> {
    args: &'a mut Vec<String>,
}

impl<'a> QemuArgsMut<'a> {
    fn new(args: &'a mut Vec<String>) -> Self {
        Self { args }
    }

    fn set_option_value(&mut self, option: &str, value: String) {
        if let Some(index) = self.args.iter().position(|arg| arg == option)
            && let Some(existing) = self.args.get_mut(index + 1)
        {
            *existing = value;
            return;
        }

        self.args.push(option.to_string());
        self.args.push(value);
    }
}

pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    QemuArgsMut::new(&mut qemu.args).set_option_value("-smp", cpu_num.to_string());
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DynamicPlatformBootArch {
    X86_64,
    LoongArch64,
}

pub(crate) fn apply_dynamic_platform_qemu_boot(qemu: &mut QemuConfig, cargo: &Cargo) {
    apply_dynamic_platform_qemu_boot_with_kvm_probe(qemu, cargo, host_kvm_available);
}

pub(super) fn apply_dynamic_platform_qemu_boot_with_kvm_probe(
    qemu: &mut QemuConfig,
    cargo: &Cargo,
    kvm_available: impl FnOnce() -> bool,
) {
    apply_x86_64_kvm_accel_if_available_with_probe(qemu, cargo, kvm_available);

    let Some(arch) = cargo_dynamic_platform_boot_arch(cargo) else {
        return;
    };

    qemu.uefi = true;
    qemu.to_bin = true;
    apply_drive_snapshot_without_global_snapshot(qemu);

    if arch != DynamicPlatformBootArch::X86_64 {
        return;
    }

    ensure_uefi_drive_bus(qemu);
    keep_qemu_default_devices_for_uefi(qemu);
    disable_unneeded_default_x86_64_devices(qemu);
    disable_dynamic_x86_64_five_level_paging(qemu);
    enable_dynamic_x86_64_nested_virtualization_features(qemu, cargo);
    apply_dynamic_x86_64_qemu_debug_args(qemu);
}

pub(super) fn apply_x86_64_kvm_accel_if_available_with_probe(
    qemu: &mut QemuConfig,
    cargo: &Cargo,
    kvm_available: impl FnOnce() -> bool,
) {
    if !cargo_target_is_x86_64(&cargo.target) {
        return;
    }
    if qemu.args.iter().any(|arg| arg == "-accel") {
        return;
    }
    if !kvm_available() {
        return;
    }

    qemu.args.push("-accel".to_string());
    qemu.args.push("kvm".to_string());
}

#[cfg(unix)]
pub(super) fn host_kvm_available() -> bool {
    use std::fs::OpenOptions;

    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

#[cfg(not(unix))]
pub(super) fn host_kvm_available() -> bool {
    false
}

pub(super) fn apply_dynamic_x86_64_qemu_debug_args(qemu: &mut QemuConfig) {
    let Ok(value) = std::env::var(DYNAMIC_X86_64_QEMU_DEBUG_ENV) else {
        return;
    };
    if !env_flag_enabled(&value) {
        return;
    }

    push_unique_arg(&mut qemu.args, "-no-reboot");
    push_unique_arg(&mut qemu.args, "-S");
    push_unique_arg(&mut qemu.args, "-s");
}

pub(super) fn env_flag_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

pub(super) fn push_unique_arg(args: &mut Vec<String>, arg: &str) {
    if !args.iter().any(|existing| existing == arg) {
        args.push(arg.to_string());
    }
}

pub(super) fn ensure_uefi_drive_bus(qemu: &mut QemuConfig) {
    for index in 0..qemu.args.len() {
        if qemu.args.get(index).is_some_and(|arg| arg == "-machine")
            && let Some(machine) = qemu.args.get_mut(index + 1)
        {
            remove_machine_option(machine, "sata=off");
            remove_machine_option(machine, "i8042=off");
        }
    }
}

pub(super) fn remove_machine_option(machine: &mut String, option: &str) {
    let parts = machine
        .split(',')
        .filter(|part| *part != option)
        .collect::<Vec<_>>();
    *machine = parts.join(",");
}

pub(super) fn keep_qemu_default_devices_for_uefi(qemu: &mut QemuConfig) {
    qemu.args.retain(|arg| arg != "-nodefaults");
}

pub(super) fn disable_unneeded_default_x86_64_devices(qemu: &mut QemuConfig) {
    let has_network_arg = qemu
        .args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-net" | "-netdev" | "-nic"));
    if !has_network_arg {
        qemu.args.push("-net".to_string());
        qemu.args.push("none".to_string());
    }

    let has_vga_arg = qemu.args.iter().any(|arg| arg == "-vga");
    if !has_vga_arg {
        qemu.args.push("-vga".to_string());
        qemu.args.push("none".to_string());
    }
}

pub(super) fn disable_dynamic_x86_64_five_level_paging(qemu: &mut QemuConfig) {
    for index in 0..qemu.args.len() {
        if qemu.args.get(index).is_some_and(|arg| arg == "-cpu")
            && let Some(cpu) = qemu.args.get_mut(index + 1)
        {
            disable_qemu_cpu_feature(cpu, "la57");
        }
    }
}

pub(super) fn enable_dynamic_x86_64_nested_virtualization_features(
    qemu: &mut QemuConfig,
    cargo: &Cargo,
) {
    for index in 0..qemu.args.len() {
        if qemu.args.get(index).is_some_and(|arg| arg == "-cpu")
            && let Some(cpu) = qemu.args.get_mut(index + 1)
        {
            for feature in dynamic_x86_64_nested_virtualization_features(cargo) {
                enable_qemu_cpu_feature(cpu, feature);
            }
        }
    }
}

pub(super) fn dynamic_x86_64_nested_virtualization_features(
    cargo: &Cargo,
) -> &'static [&'static str] {
    if cargo.features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "svm" | "axvm/svm" | "x86_vcpu/svm" | "x86-vcpu/svm"
        )
    }) {
        &["svm", "npt", "nrip-save"]
    } else if cargo.features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "vmx" | "axvm/vmx" | "x86_vcpu/vmx" | "x86-vcpu/vmx"
        )
    }) {
        &["vmx-ept", "vmx-unrestricted-guest", "vmx-flexpriority"]
    } else {
        &[]
    }
}

pub(super) fn enable_qemu_cpu_feature(cpu: &mut String, feature: &str) {
    let enabled_feature = format!("+{feature}");
    if cpu.split(',').any(|part| part.trim() == enabled_feature) {
        return;
    }

    if !cpu.is_empty() {
        cpu.push(',');
    }
    cpu.push_str(&enabled_feature);
}

pub(super) fn disable_qemu_cpu_feature(cpu: &mut String, feature: &str) {
    let disabled_feature = format!("-{feature}");
    if cpu.split(',').any(|part| part.trim() == disabled_feature) {
        return;
    }

    if !cpu.is_empty() {
        cpu.push(',');
    }
    cpu.push_str(&disabled_feature);
}

pub(crate) fn apply_drive_snapshot_without_global_snapshot(qemu: &mut QemuConfig) {
    let mut global_snapshot = false;
    qemu.args.retain(|arg| {
        let keep = arg != "-snapshot";
        if !keep {
            global_snapshot = true;
        }
        keep
    });
    if !global_snapshot {
        return;
    }

    for index in 0..qemu.args.len() {
        if qemu.args.get(index).is_some_and(|arg| arg == "-drive")
            && let Some(drive) = qemu.args.get_mut(index + 1)
        {
            ensure_drive_snapshot_on(drive);
        }
    }
}

pub(super) fn ensure_drive_snapshot_on(drive: &mut String) {
    let mut replaced = false;
    let parts = drive
        .split(',')
        .map(|part| {
            if part.starts_with("snapshot=") {
                replaced = true;
                "snapshot=on".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>();
    if replaced {
        *drive = parts.join(",");
    } else {
        drive.push_str(",snapshot=on");
    }
}

pub(super) fn cargo_dynamic_platform_boot_arch(cargo: &Cargo) -> Option<DynamicPlatformBootArch> {
    if !cargo_dynamic_platform_features(cargo).any(dynamic_platform_feature) {
        return None;
    }

    if cargo_target_is_dynamic_x86_64(&cargo.target) {
        Some(DynamicPlatformBootArch::X86_64)
    } else if cargo_target_is_dynamic_loongarch64(&cargo.target) {
        Some(DynamicPlatformBootArch::LoongArch64)
    } else {
        None
    }
}

pub(super) fn cargo_target_is_dynamic_x86_64(target: &str) -> bool {
    cargo_target_is_x86_64(target)
}

pub(super) fn cargo_target_is_x86_64(target: &str) -> bool {
    let target = target.strip_suffix(".json").unwrap_or(target);
    target.ends_with("x86_64-unknown-none") || target.ends_with("x86_64-unknown-linux-musl")
}

pub(super) fn cargo_target_is_dynamic_loongarch64(target: &str) -> bool {
    let target = target.strip_suffix(".json").unwrap_or(target);
    target.ends_with("loongarch64-unknown-none-softfloat")
        || target.ends_with("loongarch64-unknown-linux-musl")
}

pub(super) fn dynamic_platform_feature(feature: &str) -> bool {
    matches!(
        feature,
        "plat-dyn"
            | "ax-feat/plat-dyn"
            | "ax-std/plat-dyn"
            | "ax-hal/plat-dyn"
            | "ax-libc/plat-dyn"
            | "dyn-plat"
            | "starry-kernel/plat-dyn"
    )
}

pub(super) fn cargo_dynamic_platform_features(cargo: &Cargo) -> impl Iterator<Item = &str> {
    cargo.features.iter().map(String::as_str).chain(
        cargo
            .env
            .get("ARCEOS_RUST_FEATURES")
            .into_iter()
            .flat_map(|features| {
                features
                    .split(',')
                    .map(str::trim)
                    .filter(|feature| !feature.is_empty())
            }),
    )
}

pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let args = QemuArgs::new(&qemu.args);
    let value = args.option_value("-smp")?;
    parse_smp_qemu_value(value)
}

pub(super) fn parse_smp_qemu_value(value: &str) -> Option<usize> {
    let first = value.split(',').next()?;
    if let Ok(cpu_num) = first.parse() {
        return Some(cpu_num);
    }

    value.split(',').find_map(|part| {
        let cpu_num = part.strip_prefix("cpus=")?;
        cpu_num.parse().ok()
    })
}

pub(crate) fn apply_timeout_scale(qemu: &mut QemuConfig) {
    let Some(timeout) = qemu.timeout else {
        return;
    };
    if timeout == 0 {
        return;
    }

    let scale = match std::env::var(TIMEOUT_SCALE_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(scale) if scale > 1 => scale,
            Ok(_) | Err(_) => {
                eprintln!(
                    "warning: ignoring invalid {TIMEOUT_SCALE_ENV} value `{}`; expected integer > \
                     1",
                    value.trim()
                );
                return;
            }
        },
        Err(_) => return,
    };

    qemu.timeout = timeout.checked_mul(scale).or(Some(u64::MAX));
}

pub(crate) fn qemu_timeout_summary(qemu: &QemuConfig) -> String {
    match qemu.timeout {
        Some(0) | None => "disabled".to_string(),
        Some(timeout) => format!("{timeout}s"),
    }
}
