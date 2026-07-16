use std::{fs, path::Path};

#[test]
fn active_scope_access_cannot_export_an_unpinned_reference() {
    // Pull the CPU-area prefix object from the dependency archive so the
    // host-test linker assertion can validate template offset zero.
    let _ = ax_percpu::init();

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let item = fs::read_to_string(manifest_dir.join("src/item.rs"))
        .expect("scope-local item source must be readable");
    let scope = fs::read_to_string(manifest_dir.join("src/scope.rs"))
        .expect("scope-local scope source must be readable");

    assert!(
        !item.contains("impl<T> Deref for LocalItem<T>"),
        "LocalItem::Deref can return a current-CPU reference after its pin is gone"
    );
    assert!(
        item.contains("pub fn with<R>(") && item.contains("for<'access> FnOnce(&'access T) -> R"),
        "current access must use a higher-ranked, non-escaping closure"
    );
    assert!(
        item.contains("pub fn with_pinned<R>(") && item.contains("pin: &CpuPin"),
        "callers that already own a guard need an explicit CpuPin capability path"
    );
    assert!(
        item.contains("pub fn try_with_pinned<R>(") && scope.contains("fn try_get(&self)"),
        "hard-IRQ access must avoid lazy allocation and return None before initialization"
    );
    assert!(
        item.contains("PreemptGuard::new()"),
        "ordinary current access must pin the CPU for the complete closure"
    );
    assert!(
        !scope.contains("ACTIVE_SCOPE_PTR.read_current()")
            && !scope.contains("ACTIVE_SCOPE_PTR.write_current(0)")
            && !scope.contains("ACTIVE_SCOPE_PTR.write_current(scope"),
        "every active-scope per-CPU read or write must carry an explicit pin"
    );
    assert!(
        !scope.contains("current_ptr_unchecked")
            && !scope.contains("read_current_raw")
            && !scope.contains("write_current_raw"),
        "scope-local must not hide unchecked current-CPU access"
    );
    assert!(
        !scope.contains("impl Deref for ScopeCellReadGuard")
            && item.contains("pub fn scope_cell<'scope>("),
        "a ScopeCell read capability must not permit recursive gate acquisition through Scope"
    );
}
