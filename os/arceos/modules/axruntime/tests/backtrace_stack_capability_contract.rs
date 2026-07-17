use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("axruntime must live below the workspace os directory")
        .to_path_buf()
}

fn source(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn backtrace_walks_use_a_fresh_exact_stack_capability() {
    let backtrace = source("components/axbacktrace/src/lib.rs");
    assert!(backtrace.contains("InstalledStackBounds::Provider"));
    assert!(backtrace.contains("resolve_stack_bounds"));
    assert!(!backtrace.contains("static FP_RANGE"));

    let runtime = source("os/arceos/modules/axruntime/src/lib.rs");
    assert!(runtime.contains("current_backtrace_stack_bounds"));
    assert!(runtime.contains("task::current_kernel_stack_bounds()"));
    assert!(runtime.contains("ax_hal::mem::boot_stack_bounds(cpu)"));
    assert!(runtime.contains("axbacktrace::init_with_stack_provider"));
    assert!(runtime.contains("current_backtrace_stack_bounds,"));
}

#[test]
fn fatal_stack_lookup_does_not_reenter_lock_runtime() {
    let task = source("os/arceos/modules/axruntime/src/task.rs");
    let provider = task
        .split_once("pub(crate) fn current_kernel_stack_bounds()")
        .expect("runtime stack provider must exist")
        .1
        .split_once("unsafe fn prepare_current_runtime_context_publish")
        .expect("runtime stack provider must remain focused")
        .0;
    assert!(provider.contains("ax_hal::asm::disable_irqs()"));
    assert!(provider.contains("RuntimeStack"));
    assert!(provider.contains("usable_bounds()"));
    assert!(!provider.contains("IrqGuard::new"));
    assert!(!provider.contains("thread_handle"));

    let panic = source("os/arceos/modules/axruntime/src/lang_items.rs");
    let handler = panic
        .split_once("fn panic(info: &PanicInfo) -> !")
        .expect("freestanding panic handler must exist")
        .1
        .split_once("fn panic_primary")
        .expect("panic entry must remain focused")
        .0;
    let disable = handler
        .find("ax_hal::asm::disable_irqs()")
        .expect("panic entry must pin the fatal context");
    let claim = handler
        .find("axpanic::enter_panic")
        .expect("panic entry must claim global ownership");
    assert!(disable < claim);
}
