use std::{fs, path::PathBuf};

fn percpu_source() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest_dir.join("src/percpu.rs"))
        .expect("failed to read the ax-plat CPU-local implementation")
}

#[test]
fn current_cpu_identity_has_one_immutable_owner() {
    let source = percpu_source();

    assert!(
        !source.contains("static CPU_ID"),
        "logical CPU identity must not be duplicated in mutable per-CPU storage"
    );

    let helper_start = source
        .find("pub fn this_cpu_id_pinned")
        .expect("ax-plat must expose a pinned current-CPU query");
    let helper = &source[helper_start..];
    let helper_end = helper
        .find("\n}\n")
        .expect("failed to locate the pinned current-CPU query body");
    let helper = &helper[..helper_end];

    assert!(
        helper.contains("ax_percpu::bound_current(pin)")
            && helper.contains("ax_percpu::current_cpu_index(&bound_pin)"),
        "logical CPU identity must come from a layout-verified current-area capability"
    );
}
