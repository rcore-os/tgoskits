use std::{fs, path::Path};

#[test]
fn platform_boundary_is_static_value_only_and_has_one_dynamic_implementation() {
    let workspace = workspace_root();
    let interface = read(&workspace, "components/ax-cpu-local/src/abi.rs");
    let implementation = read(&workspace, "platforms/axplat-dyn/src/percpu.rs");

    assert!(interface.contains("pub trait CpuLocalPlatformV1"));
    let body = interface
        .split_once("pub trait CpuLocalPlatformV1")
        .expect("CPU-local platform interface must exist")
        .1
        .split_once('}')
        .expect("CPU-local platform interface must have a body")
        .0;
    for operation in ["current_cpu_binding", "current_thread", "get_tp", "set_tp"] {
        assert!(
            body.contains(operation),
            "platform ABI is missing {operation}"
        );
    }
    for forbidden in ['&', '*'] {
        assert!(
            !body.contains(forbidden),
            "platform ABI must carry integer values, not Rust pointers or references"
        );
    }
    assert!(implementation.contains("impl CpuLocalPlatformV1 for CpuLocalPlatform"));
    assert!(!interface.contains("publish_current_thread"));
}

#[test]
fn axhal_facade_owns_pinning_and_typed_conversion() {
    let workspace = workspace_root();
    let facade = read(&workspace, "os/arceos/modules/axhal/src/percpu.rs");
    let leaf = read(&workspace, "components/ax-cpu-local/src/register.rs");
    let leaf_exports = read(&workspace, "components/ax-cpu-local/src/lib.rs");
    let lib = read(&workspace, "os/arceos/modules/axhal/src/lib.rs");

    for api in [
        "pub fn cpu_base(",
        "pub fn current_thread(",
        "pub unsafe fn prepare_current_thread_publish",
        "pub unsafe fn commit_current_thread_publish(",
        "pub unsafe fn install_bootstrap_current_thread(",
        "pub fn kernel_tls()",
        "pub unsafe fn install_bootstrap_kernel_tls(",
    ] {
        assert!(
            facade.contains(api),
            "ax-hal typed facade is missing `{api}`"
        );
    }
    for source in [&facade, &leaf, &leaf_exports] {
        assert!(
            !source.contains("pub unsafe fn publish_current_thread(")
                && !source.contains("publish_current_thread_for_binding"),
            "one-shot current-thread publication must not bypass the two-phase switch contract",
        );
    }
    assert!(!facade.contains("def_percpu"));
    assert!(!facade.contains("CURRENT_TASK_PTR"));
    assert!(!facade.contains("write_thread_pointer"));
    assert!(lib.contains("cfg(all(feature = \"uspace\", feature = \"tls\"))"));
    assert!(lib.contains("compile_error!"));
}

#[test]
fn platform_entry_only_validates_the_frozen_layout_before_binding() {
    let workspace = workspace_root();
    let boot = read(&workspace, "platforms/axplat-dyn/src/boot.rs");
    let binding = read(&workspace, "platforms/somehal/src/setup.rs");

    assert!(!boot.contains("fn install_percpu_layout()"));
    assert!(boot.contains("ax_percpu::layout()"));
    assert!(boot.contains("somehal::smp::percpu_data_layout()"));
    assert!(binding.contains("as *const ax_cpu_local::CpuAreaHeader"));
    assert!(binding.contains("}.binding()"));
    assert!(
        !binding.contains("CpuBindingV1 {"),
        "somehal must return the complete frozen header binding instead of reconstructing fields",
    );
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ax-plat must be nested under the workspace platforms directory")
        .to_path_buf()
}

fn read(workspace: &Path, relative: &str) -> String {
    fs::read_to_string(workspace.join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}
