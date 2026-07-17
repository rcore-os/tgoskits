//! Source-level contracts for evdev IRQ activation and rollback.

const EVENT: &str = include_str!("../src/pseudofs/dev/event.rs");

fn function_body<'source>(source: &'source str, signature: &str, next_item: &str) -> &'source str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"))
        .1
        .split_once(next_item)
        .unwrap_or_else(|| panic!("missing item after {signature}: {next_item}"))
        .0
}

#[test]
fn driver_irq_activation_uses_an_explicit_exclusive_phase() {
    let register = function_body(EVENT, "fn register_irq(", "fn request_polling(");

    assert!(
        EVENT.contains("enum IrqActivationState"),
        "evdev IRQ setup must expose a typed activation state"
    );
    assert!(
        register.contains("with_activation_device("),
        "driver enable/disable callbacks must run through the exclusive activation helper"
    );
    assert!(
        !register.contains("self.inner.lock().device.enable_irq()")
            && !register.contains("self.inner.lock().device.disable_irq()"),
        "external driver callbacks must never execute while SpinNoIrq protects evdev state"
    );
}

#[test]
fn failed_irq_activation_releases_the_disabled_action_before_polling_fallback() {
    let register = function_body(EVENT, "fn register_irq(", "fn request_polling(");
    let rollback = function_body(
        EVENT,
        "fn rollback_requested_irq(",
        "/// Runs a driver activation callback",
    );
    let disable = rollback
        .find("disable_irq(handle)")
        .expect("an enabled action must be disabled during rollback");
    let synchronize = rollback
        .find("synchronize_irq(handle)")
        .expect("rollback must drain an action before releasing its handler ownership");
    let driver_disable = rollback
        .find("with_activation_device(|device| device.disable_irq())")
        .expect("the hardware source must be disabled outside the evdev spinlock");
    let free = rollback
        .find("free_irq(handle)")
        .expect("every post-request activation failure must release the IRQ action");
    assert!(
        disable < synchronize && synchronize < driver_disable && driver_disable < free,
        "rollback order must be action disable, drain, driver disable, then ownership release"
    );
    assert!(
        rollback.contains("IrqActivationState::Quarantined")
            && rollback.contains("self.irq_handle.call_once(|| handle)"),
        "a failed free must retain the disabled action token in a typed quarantine state"
    );
    register
        .rfind("self.start_polling()")
        .expect("polling fallback must start after IRQ activation reaches a terminal state");

    assert!(
        register.contains("activate_requested_irq(irq, handle)"),
        "post-request failures must flow through the synchronized rollback helper"
    );
    assert_eq!(
        register.matches("self.start_polling()").count(),
        1,
        "polling must start exactly once, after activation has committed or rolled back"
    );
}
