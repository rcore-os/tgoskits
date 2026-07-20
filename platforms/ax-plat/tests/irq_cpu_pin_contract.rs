use std::{fs, path::PathBuf};

fn irq_source() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest_dir.join("src/irq.rs"))
        .expect("failed to read the ax-plat IRQ implementation")
}

fn library_source() -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest_dir.join("src/lib.rs"))
        .expect("failed to read the ax-plat library implementation")
}

#[test]
fn current_irq_marker_query_keeps_the_cpu_pinned() {
    let source = irq_source();
    let helper_start = source
        .find("fn current_cpu_in_irq_context() -> bool")
        .expect("IRQ context queries need one shared pinned-current-CPU helper");
    let helper = &source[helper_start..];
    let helper_end = helper
        .find("\n}\n")
        .expect("failed to locate the pinned-current-CPU helper body");
    let helper = &helper[..helper_end];

    let guard = helper
        .find("IrqGuard::new()")
        .expect("the IRQ marker query must mask migration-capable IRQ return");
    let cpu = helper
        .find("this_cpu_id_pinned(guard.cpu_pin())")
        .expect("the IRQ marker query must read the CPU ID through the same pin");
    let marker = helper
        .find("in_irq_context_on")
        .expect("the IRQ marker must be sampled before the CPU pin is released");

    assert!(guard < cpu && cpu < marker);
    assert!(
        source.matches("current_cpu_in_irq_context()").count() >= 3,
        "the public query and IrqOps adapter must share the pinned implementation"
    );
}

#[test]
fn migration_hook_cannot_run_between_cpu_and_marker_reads() {
    let source = irq_source();
    let helper_start = source
        .find("fn current_cpu_in_irq_context() -> bool")
        .expect("IRQ context queries need one shared pinned-current-CPU helper");
    let helper = &source[helper_start..];
    let helper_end = helper
        .find("\n}\n")
        .expect("failed to locate the guard-lifetime helper body");
    let helper = &helper[..helper_end];

    let cpu = helper
        .find("this_cpu_id_pinned(guard.cpu_pin())")
        .expect("the CPU must be sampled while the pin guard is live");
    let query = helper
        .find("let result = in_irq_context_on(cpu);")
        .expect("the CPU-owned marker must be sampled while the same guard is live");
    let release = helper
        .find("drop(guard);")
        .expect("the migration-capable guard release must be explicit");

    assert!(cpu < query && query < release);
}

#[test]
fn cpu_owned_thunk_uses_one_pinned_local_decision() {
    let source = irq_source();
    let method_start = source
        .find("    fn run_on_cpu_sync(\n        &self,")
        .expect("failed to locate the platform CPU-owned execution adapter");
    let method = &source[method_start..];
    let method_end = method
        .find("\n    fn prepare_line(")
        .expect("failed to locate the end of the CPU-owned execution adapter");
    let method = &method[..method_end];

    assert!(
        !method.contains("self.current_cpu()"),
        "CPU-owned execution must not branch on an escaping CPU snapshot"
    );
    let preempt_pin = method
        .find("PreemptGuard::new()")
        .expect("remote handoff must remain pinned after local IRQs are restored");
    let irq_pin = method
        .find("IrqGuard::new()")
        .expect("the local CPU decision must be protected from IRQ-return migration");
    let cpu = method
        .find("this_cpu_id_pinned(irq_guard.cpu_pin())")
        .expect("the current CPU must be read through the IRQ guard's pin");
    let local = method
        .find("if cpu == current_cpu")
        .expect("the pinned adapter must recognize a local target");
    let thunk = method
        .find("unsafe { f(arg) }")
        .expect("the local thunk must execute directly under the same pin");
    let release = method
        .find("drop(irq_guard)")
        .expect("the local IRQ pin release must be explicit");
    let irq_context = method
        .find("in_irq_context_on(current_cpu)")
        .expect("remote IRQ-context rejection must use the pinned CPU identity");
    let reject = method
        .find("Err(IrqError::InIrqContext)")
        .expect("remote execution from IRQ context must fail explicitly");

    assert!(preempt_pin < irq_pin && irq_pin < cpu);
    assert!(cpu < local && local < thunk && thunk < release);
    assert!(cpu < irq_context && irq_context < reject);
}

#[test]
fn synchronous_cpu_hook_is_unsafe_and_cannot_be_replaced() {
    let source = irq_source();
    let setter = source
        .find("pub unsafe fn set_run_on_cpu_sync")
        .expect("the raw thunk hook installation must require an unsafe proof");
    let safety = source[..setter]
        .rfind("/// # Safety")
        .expect("the unsafe hook installation must document its lifetime contract");
    assert!(source.contains("crate::install_runtime_hook_once("));

    let library = library_source();
    let installer = library
        .find("pub(crate) fn install_runtime_hook_once")
        .expect("the hook needs a one-shot installer");
    let installer = &library[installer..];
    let installer_end = installer
        .find("\n}\n")
        .expect("failed to locate the one-shot installer body");
    let installer = &installer[..installer_end];

    assert!(safety < setter);
    assert!(installer.contains("compare_exchange(0, candidate"));
    assert!(installer.contains("installed == candidate"));
    assert!(!installer.contains(".store("));
}
