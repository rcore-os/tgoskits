//! Source-level regression guards for the unsafe IRQ adapter boundary.

const TYPES: &str = include_str!("../src/types.rs");
const REGISTRY: &str = include_str!("../src/registry.rs");

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
