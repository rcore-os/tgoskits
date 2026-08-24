use std::{fs, path::PathBuf};

#[test]
fn irq_route_registry_never_uses_preempt_only_locking() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/irq.rs")).unwrap();
    let compact: String = source.chars().filter(|ch| !ch.is_whitespace()).collect();

    assert!(
        compact.contains("IRQ_ROUTES.lock_irqsave()"),
        "IRQ route access must have an IRQ-save locking path"
    );
    for forbidden in [
        "IRQ_ROUTES.lock()",
        "IRQ_ROUTES.try_lock()",
        "IRQ_ROUTES.lock_raw()",
        "IRQ_ROUTES.try_lock_raw()",
    ] {
        assert!(
            !compact.contains(forbidden),
            "IRQ route access must use the IRQ-save helper, found {forbidden}"
        );
    }
}
