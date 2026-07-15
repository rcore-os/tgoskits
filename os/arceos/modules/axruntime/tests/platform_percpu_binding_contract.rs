//! Source-level contract for the unique platform CPU-area binder.

use std::{fs, path::Path};

const SOMEBOOT: &str = include_str!("../../../../../platforms/someboot/src/lib.rs");
const SOMEBOOT_SMP: &str = include_str!("../../../../../platforms/someboot/src/smp/mod.rs");
const DYNAMIC_BOOT: &str = include_str!("../../../../../platforms/axplat-dyn/src/boot.rs");
const DYNAMIC_MEMORY: &str = include_str!("../../../../../platforms/axplat-dyn/src/mem.rs");
const PLATFORM_PERCPU: &str = include_str!("../../../../../platforms/ax-plat/src/percpu.rs");

#[test]
fn platform_entry_is_the_only_runtime_cpu_area_binder() {
    assert!(
        !SOMEBOOT.contains("init_runtime_percpu_reg"),
        "someboot must hand boot identity to the platform without installing the runtime anchor",
    );
    assert!(
        SOMEBOOT_SMP.contains("__ax_percpu_initialize_layout_v2("),
        "someboot final-high must construct and freeze the CPU-area layout before platform entry",
    );
    assert!(
        DYNAMIC_BOOT.contains("ax_cpu_local::raw::install_binding(binding)"),
        "the selected platform entry must bind the validated area through the leaf raw primitive",
    );
    assert!(
        !DYNAMIC_BOOT.contains("ax_percpu::bind_current"),
        "ax-percpu must remain the pure-Rust layout layer, not the register binder",
    );
    assert!(
        !DYNAMIC_MEMORY.contains("_percpu_base_ptr"),
        "remote CPU lookup must use the immutable registered layout, not an extern callback",
    );
    assert!(
        !PLATFORM_PERCPU.contains("ax_percpu::init()")
            && !PLATFORM_PERCPU.contains("init_percpu_reg"),
        "ax-runtime initialization may verify a platform binding but must not bind it again",
    );

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let binders = production_call_sites(&workspace, "ax_cpu_local::raw::install_binding(");
    assert_eq!(
        binders,
        ["platforms/axplat-dyn/src/boot.rs"],
        "every production source directory must retain exactly one CPU-area binder",
    );
    let layout_initializers =
        production_call_sites(&workspace, "__ax_percpu_initialize_layout_v2(");
    assert_eq!(
        layout_initializers,
        [
            "components/percpu/percpu/src/initialization.rs",
            "platforms/someboot/src/smp/mod.rs",
        ],
        "the layout ABI must retain one semantic definition and one final-high boot caller",
    );
}

fn production_call_sites(workspace: &Path, needle: &str) -> Vec<String> {
    let mut matches = Vec::new();
    for root in [
        "components",
        "drivers",
        "memory",
        "net",
        "os",
        "platforms",
        "virtualization",
    ] {
        visit_rust_sources(workspace, &workspace.join(root), needle, &mut matches);
    }
    matches.sort();
    matches
}

fn visit_rust_sources(workspace: &Path, directory: &Path, needle: &str, matches: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("production source directory must be readable") {
        let path = entry
            .expect("production source entry must be readable")
            .path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            visit_rust_sources(workspace, &path, needle, matches);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs")
            || !path
                .components()
                .any(|component| component.as_os_str() == "src")
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("production Rust source must be UTF-8");
        let production = source
            .split_once("#[cfg(test)]\nmod tests")
            .map_or(source.as_str(), |(production, _)| production);
        if production.contains(needle) {
            matches.push(
                path.strip_prefix(workspace)
                    .expect("source must stay inside the workspace")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}
