//! Source-level contract for the unique platform CPU-area binder.

use std::{fs, path::Path};

const SOMEBOOT: &str = include_str!("../../../../../platforms/someboot/src/lib.rs");
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
        DYNAMIC_BOOT.contains("ax_percpu::install_layout")
            && DYNAMIC_BOOT.contains("ax_percpu::bind_current"),
        "the selected platform entry must install and bind the CPU-area layout",
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
    let binders = production_call_sites(&workspace, "ax_percpu::bind_current(");
    assert_eq!(
        binders,
        ["platforms/axplat-dyn/src/boot.rs"],
        "every production source directory must retain exactly one CPU-area binder",
    );
    let layout_installers = production_call_sites(&workspace, "ax_percpu::install_layout(");
    assert_eq!(
        layout_installers,
        ["platforms/axplat-dyn/src/boot.rs"],
        "the platform binder must also be the only runtime layout installer",
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
        if source.contains(needle) {
            matches.push(
                path.strip_prefix(workspace)
                    .expect("source must stay inside the workspace")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}
