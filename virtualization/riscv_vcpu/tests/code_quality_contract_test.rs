use std::{fs, path::Path};

#[test]
fn runtime_core_has_no_placeholder_or_unchecked_paths() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = ["src/vcpu.rs", "src/percpu.rs", "src/detect.rs"];
    let forbidden = ["todo!(", "unimplemented!(", "unwrap_unchecked(", "panic!("];

    let mut violations = Vec::new();
    for file in files {
        let path = manifest_dir.join(file);
        let content = fs::read_to_string(&path).unwrap();
        for pattern in forbidden {
            if content.contains(pattern) {
                violations.push(format!("{file} contains `{pattern}`"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "RISC-V vCPU runtime core must return typed errors instead of placeholder/panic paths: \
         {violations:?}"
    );
}

#[test]
fn vpmu_comments_do_not_reference_axvm_trait_hooks() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let content = fs::read_to_string(manifest_dir.join("src/vpmu.rs")).unwrap();

    assert!(
        !content.contains("VmArchVcpuOps::"),
        "riscv_vcpu comments should describe core hooks, not AxVM trait hooks"
    );
}

#[test]
fn tests_scan_only_existing_runtime_files() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [
        "src/vcpu.rs",
        "src/percpu.rs",
        "src/detect.rs",
        "src/vpmu.rs",
    ] {
        assert!(
            Path::new(&manifest_dir.join(file)).exists(),
            "{file} moved; update contract tests"
        );
    }
}
