use std::path::Path;

fn src_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    })
}

fn src_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

#[test]
fn loongarch_vcpu_is_gated_at_crate_root() {
    let lib = src_file("lib.rs");

    assert!(
        lib.contains("#![cfg(target_arch = \"loongarch64\")]"),
        "loongarch_vcpu should use a crate-level loongarch64 cfg gate"
    );
    assert!(
        !lib.contains("#[cfg(not(target_arch = \"loongarch64\"))]"),
        "loongarch_vcpu should not expose non-loongarch64 fallback APIs"
    );
}

#[test]
fn loongarch_vcpu_has_no_internal_target_arch_cfg() {
    for entry in std::fs::read_dir(src_dir()).expect("read loongarch_vcpu src dir") {
        let entry = entry.expect("read src dir entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }

        let name = path.file_name().and_then(|name| name.to_str()).unwrap();
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let source_without_crate_gate = if name == "lib.rs" {
            source.replace("#![cfg(target_arch = \"loongarch64\")]", "")
        } else {
            source
        };

        for forbidden in [
            "cfg(target_arch = \"loongarch64\")",
            "cfg(not(target_arch = \"loongarch64\"))",
            "target_arch = \"loongarch64\"",
        ] {
            assert!(
                !source_without_crate_gate.contains(forbidden),
                "{} should not contain internal target cfg: {forbidden}",
                path.display()
            );
        }
    }
}

#[test]
fn exception_module_is_only_the_exception_dispatcher() {
    let exception = src_file("exception.rs");
    let line_count = exception.lines().count();

    assert!(
        line_count <= 800,
        "exception.rs should stay focused on exception dispatch, got {line_count} lines"
    );

    for forbidden in [
        "struct LoongArchIocsrState",
        "fn read_guest_csr",
        "fn write_guest_csr",
        "fn host_iocsr_read",
        "fn direct_map_guest_addr_to_gpa",
        "const CSR_CRMD",
        "const EIOINTC_ISR_BASE",
    ] {
        assert!(
            !exception.contains(forbidden),
            "exception.rs still owns a split-out responsibility: {forbidden}"
        );
    }
}

#[test]
fn loongarch_vcpu_core_has_responsibility_focused_modules() {
    for module in [
        "trap.rs",
        "guest_addr.rs",
        "host_cpu.rs",
        "guest_csr.rs",
        "iocsr.rs",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join(module);
        assert!(
            path.exists(),
            "loongarch_vcpu should split {module} out of exception dispatch"
        );
    }
}
