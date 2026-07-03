use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{ENV_LOCK, TempEnvVar};
use crate::test::qemu::{DYNAMIC_X86_64_QEMU_DEBUG_ENV, boot::*};

#[test]
fn dynamic_x86_64_cargo_uses_uefi_bin_qemu_boot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(qemu.uefi);
    assert!(qemu.to_bin);
}

#[test]
fn dynamic_x86_64_std_cargo_uses_uefi_bin_qemu_boot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(qemu.uefi);
    assert!(qemu.to_bin);
}

#[test]
fn axvisor_x86_64_uses_dependency_dynamic_platform_boot_without_feature() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        package: "axvisor".to_string(),
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        features: vec![],
        to_bin: false,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(qemu.uefi);
    assert!(qemu.to_bin);
}

#[test]
fn axvisor_loongarch64_uses_dependency_dynamic_platform_boot_without_feature() {
    let cargo = Cargo {
        package: "axvisor".to_string(),
        target: "scripts/targets/std/pie/loongarch64-unknown-linux-musl.json".to_string(),
        features: vec![],
        to_bin: false,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        args: vec!["-snapshot".to_string()],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(qemu.uefi);
    assert!(qemu.to_bin);
    assert!(qemu.args.is_empty());
}

#[test]
fn dynamic_x86_64_qemu_boot_converts_global_snapshot_to_drive_snapshots() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-nographic".to_string(),
            "-snapshot".to_string(),
            "-drive".to_string(),
            "id=disk0,format=raw,file=rootfs.img".to_string(),
            "-smp".to_string(),
            "1".to_string(),
        ],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-nographic",
            "-drive",
            "id=disk0,format=raw,file=rootfs.img,snapshot=on",
            "-smp",
            "1",
            "-net",
            "none",
            "-vga",
            "none"
        ]
    );
}

#[test]
fn dynamic_loongarch64_cargo_uses_uefi_bin_qemu_boot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "loongarch64-unknown-none-softfloat".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(qemu.uefi);
    assert!(qemu.to_bin);
}

#[test]
fn dynamic_loongarch64_std_cargo_uses_uefi_bin_qemu_boot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/loongarch64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(qemu.uefi);
    assert!(qemu.to_bin);
}

#[test]
fn dynamic_loongarch64_qemu_boot_converts_global_snapshot_to_drive_snapshots() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/loongarch64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-machine".to_string(),
            "virt".to_string(),
            "-snapshot".to_string(),
            "-drive".to_string(),
            "id=disk0,format=raw,file=rootfs.img".to_string(),
        ],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-machine",
            "virt",
            "-drive",
            "id=disk0,format=raw,file=rootfs.img,snapshot=on"
        ]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_keeps_uefi_drive_bus_available() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-machine".to_string(),
            "q35,sata=off,smbus=off,i8042=off".to_string(),
        ],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        ["-machine", "q35,smbus=off", "-net", "none", "-vga", "none"]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_keeps_default_uefi_disk_bus_available() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-nodefaults".to_string(),
            "-machine".to_string(),
            "q35,sata=off,smbus=off,i8042=off".to_string(),
        ],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        ["-machine", "q35,smbus=off", "-net", "none", "-vga", "none"]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_disables_five_level_paging_cpu_feature() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-cpu".to_string(),
            "host,+x2apic".to_string(),
            "-machine".to_string(),
            "q35".to_string(),
        ],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);
    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-cpu",
            "host,+x2apic,-la57",
            "-machine",
            "q35",
            "-net",
            "none",
            "-vga",
            "none"
        ]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_enables_vmx_nested_features_for_vmx_backend() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/pie/x86_64-unknown-none.json".to_string(),
        features: vec!["vmx".to_string()],
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-cpu".to_string(), "host".to_string()],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);
    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-cpu",
            "host,-la57,+vmx-ept,+vmx-unrestricted-guest,+vmx-flexpriority",
            "-net",
            "none",
            "-vga",
            "none"
        ]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_enables_svm_nested_features_for_svm_backend() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/pie/x86_64-unknown-none.json".to_string(),
        features: vec!["svm".to_string()],
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-cpu".to_string(), "host".to_string()],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);
    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-cpu",
            "host,-la57,+svm,+npt,+nrip-save",
            "-net",
            "none",
            "-vga",
            "none"
        ]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_keeps_explicit_network_and_vga_args() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-netdev".to_string(),
            "user,id=net0".to_string(),
            "-device".to_string(),
            "virtio-net-pci,netdev=net0".to_string(),
            "-vga".to_string(),
            "std".to_string(),
        ],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-netdev",
            "user,id=net0",
            "-device",
            "virtio-net-pci,netdev=net0",
            "-vga",
            "std"
        ]
    );
}

#[test]
fn dynamic_x86_64_qemu_boot_can_enable_debug_stub() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::set(DYNAMIC_X86_64_QEMU_DEBUG_ENV, "1");
    let cargo = Cargo {
        target: "scripts/targets/std/pie/x86_64-unknown-linux-musl.json".to_string(),
        to_bin: true,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-nographic".to_string()],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert_eq!(
        qemu.args,
        [
            "-nographic",
            "-net",
            "none",
            "-vga",
            "none",
            "-no-reboot",
            "-S",
            "-s"
        ]
    );
}

#[test]
fn non_dynamic_aarch64_cargo_keeps_existing_qemu_boot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::set(DYNAMIC_X86_64_QEMU_DEBUG_ENV, "1");
    let cargo = Cargo {
        target: "aarch64-unknown-none-softfloat".to_string(),
        features: vec![],
        to_bin: false,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(!qemu.uefi);
    assert!(!qemu.to_bin);
}

#[test]
fn non_dynamic_riscv64_cargo_keeps_existing_qemu_boot() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::set(DYNAMIC_X86_64_QEMU_DEBUG_ENV, "1");
    let cargo = Cargo {
        target: "riscv64gc-unknown-none-elf".to_string(),
        features: vec![],
        to_bin: false,
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        uefi: false,
        to_bin: false,
        args: vec!["-snapshot".to_string()],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || false);

    assert!(!qemu.uefi);
    assert!(!qemu.to_bin);
    assert_eq!(qemu.args, ["-snapshot"]);
}

#[test]
fn x86_64_qemu_uses_kvm_when_available() {
    let cargo = Cargo {
        target: "x86_64-unknown-none".to_string(),
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-nographic".to_string()],
        ..Default::default()
    };

    apply_x86_64_kvm_accel_if_available_with_probe(&mut qemu, &cargo, || true);

    assert_eq!(qemu.args, ["-nographic", "-accel", "kvm"]);
}

#[test]
fn qemu_boot_rewrite_uses_kvm_for_x86_64_when_available() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _debug = TempEnvVar::unset(DYNAMIC_X86_64_QEMU_DEBUG_ENV);
    let cargo = Cargo {
        target: "scripts/targets/std/x86_64-unknown-linux-musl.json".to_string(),
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-nographic".to_string()],
        ..Default::default()
    };

    apply_dynamic_platform_qemu_boot_with_kvm_probe(&mut qemu, &cargo, || true);

    assert_eq!(
        qemu.args,
        [
            "-nographic",
            "-accel",
            "kvm",
            "-net",
            "none",
            "-vga",
            "none"
        ]
    );
}

#[test]
fn x86_64_qemu_keeps_explicit_accel() {
    let cargo = Cargo {
        target: "x86_64-unknown-none".to_string(),
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec![
            "-nographic".to_string(),
            "-accel".to_string(),
            "tcg,thread=single".to_string(),
        ],
        ..Default::default()
    };

    apply_x86_64_kvm_accel_if_available_with_probe(&mut qemu, &cargo, || true);

    assert_eq!(qemu.args, ["-nographic", "-accel", "tcg,thread=single"]);
}

#[test]
fn non_x86_64_qemu_does_not_use_kvm() {
    let cargo = Cargo {
        target: "riscv64gc-unknown-none-elf".to_string(),
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-nographic".to_string()],
        ..Default::default()
    };

    apply_x86_64_kvm_accel_if_available_with_probe(&mut qemu, &cargo, || true);

    assert_eq!(qemu.args, ["-nographic"]);
}

#[test]
fn x86_64_qemu_does_not_use_kvm_without_permission() {
    let cargo = Cargo {
        target: "x86_64-unknown-none".to_string(),
        ..Default::default()
    };
    let mut qemu = QemuConfig {
        args: vec!["-nographic".to_string()],
        ..Default::default()
    };

    apply_x86_64_kvm_accel_if_available_with_probe(&mut qemu, &cargo, || false);

    assert_eq!(qemu.args, ["-nographic"]);
}
