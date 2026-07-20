//! Source-level regression guards for the unsafe IRQ adapter boundary.

const TYPES: &str = include_str!("../src/types.rs");
const REGISTRY: &str = include_str!("../src/registry/mod.rs");

#[test]
fn deferred_cpu_thunks_require_an_unsafe_ops_implementation() {
    assert!(
        TYPES.contains("pub unsafe trait IrqOps"),
        "IrqOps implementations must explicitly uphold the synchronous thunk lifetime contract"
    );
    assert!(
        TYPES.contains("On failure `f` must not have begun")
            && TYPES.contains("must never be invoked later"),
        "the unsafe contract must make every error a before-execution cancellation boundary"
    );
}

#[test]
fn registry_sync_requires_thread_safe_platform_ops() {
    assert!(
        REGISTRY.contains("unsafe impl<O: IrqOps + Sync> Sync for Registry<O>"),
        "Registry must not turn Send + !Sync platform operations into a Sync object"
    );
}

#[test]
fn irqchip_ownership_is_prepared_once_and_live_updates_are_infallible() {
    assert!(
        TYPES.contains("fn prepare_line(") && TYPES.contains("Result<PreparedIrqLine, IrqError>"),
        "the task-side preparation boundary must return a validated line binding"
    );
    assert!(
        TYPES.contains(
            "fn set_line_enabled(&self, binding: IrqLineBinding, cpu: Option<CpuId>, enabled: \
             bool);"
        ),
        "published line updates must use an infallible prepared endpoint"
    );
    assert!(
        !TYPES.contains("fn set_affinity(") && !TYPES.contains("fn is_enabled("),
        "live IRQ control must not rebuild affinity or snapshot controller enable state"
    );
}

#[test]
fn irqchip_release_is_an_explicit_rollback_safe_transaction() {
    assert!(
        TYPES.contains("fn release_line(&self, _binding: IrqLineBinding) -> Result<(), IrqError>"),
        "platform IRQ ownership release must cross an explicit generation-bearing boundary"
    );
    assert!(
        TYPES.contains("failure must leave that same binding usable"),
        "the platform contract must preserve the old binding on release failure"
    );
    assert!(
        REGISTRY.contains("descriptor.begin_line_release(handle.id)")
            && REGISTRY.contains("self.ops.release_line(prepared.binding())")
            && REGISTRY.contains("descriptor.rollback_line_release(prepared)"),
        "the registry must reserve, release outside metadata mutation, and roll back on failure"
    );
}

#[test]
fn drain_wake_requires_an_explicit_unsafe_callback_contract() {
    assert!(
        TYPES.contains("pub const unsafe fn new"),
        "safe code must not be able to make the framework call an unsafe callback with arbitrary \
         data"
    );
    assert!(
        TYPES.contains("The callback and `data` must remain valid"),
        "the constructor must document its hard-IRQ lifetime and concurrency contract"
    );
}

#[test]
fn hard_irq_descriptor_lookup_never_waits_for_the_registration_catalog() {
    assert!(REGISTRY.contains("descriptor_catalog: [AtomicPtr<Descriptor>;"));
    let lookup = REGISTRY
        .split_once("fn descriptor_ptr(&self, irq: IrqId)")
        .expect("registry must expose one internal descriptor lookup primitive")
        .1
        .split_once("/// Executes one descriptor-local transaction")
        .expect("descriptor lookup must remain a focused operation")
        .0;

    assert!(lookup.contains("load(Ordering::Acquire)"));
    assert!(!lookup.contains("catalog_lock.lock"));
}
