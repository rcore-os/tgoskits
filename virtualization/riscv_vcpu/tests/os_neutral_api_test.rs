use std::{fs, path::Path};

const PUBLIC_TYPES: &[&str] = &[
    "RiscvVcpuError",
    "RiscvVcpuResult",
    "RiscvVmExit",
    "RiscvGuestPhysAddr",
    "RiscvGuestVirtAddr",
    "RiscvHostPhysAddr",
    "RiscvHostVirtAddr",
    "RiscvAccessWidth",
    "RiscvAccessFlags",
    "RiscvNestedPagingConfig",
    "RiscvVmId",
    "RiscvVcpuId",
    "RiscvHostOps",
    "RiscvVcpu",
    "RiscvVCpu",
    "RISCVVCpu",
    "RiscvPerCpu",
    "RISCVPerCpu",
];

#[test]
fn riscv_vcpu_exposes_os_neutral_api_names() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = read_rust_sources(&manifest_dir.join("src"));

    for ty in PUBLIC_TYPES {
        assert!(
            source.contains(ty),
            "riscv_vcpu must expose OS-neutral public API `{ty}`"
        );
    }
}

#[test]
fn riscv_vcpu_core_does_not_mention_axvm_traits_or_exits() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = read_rust_sources(&manifest_dir.join("src"));

    for forbidden in ["VmArchVcpuOps", "VmArchPerCpuOps", "axvm_types::VmExit"] {
        assert!(
            !source.contains(forbidden),
            "riscv_vcpu core must not mention AxVM API `{forbidden}`"
        );
    }
}

fn read_rust_sources(src_dir: &Path) -> String {
    let mut combined = String::new();
    for entry in fs::read_dir(src_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            combined.push_str(&fs::read_to_string(path).unwrap());
            combined.push('\n');
        }
    }
    combined
}
