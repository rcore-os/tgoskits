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
        TYPES.contains("must not invoke `f` after this method returns"),
        "the unsafe contract must forbid deferred use of the stack-backed thunk argument"
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
