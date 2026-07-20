use std::{fs, path::PathBuf};

fn runtime_input_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/input.rs");
    fs::read_to_string(path).expect("ax-runtime must own input activation")
}

#[test]
fn runtime_owns_input_irq_activation_transaction() {
    let source = runtime_input_source();
    let owner = source
        .split_once("fn run_input_owner(")
        .expect("input runtime has a CPU-pinned owner")
        .1
        .split_once("fn prepare_input_irq_action(")
        .expect("owner body precedes IRQ registration helper")
        .0;

    let register = owner
        .find("prepare_input_irq_action")
        .expect("owner prepares a disabled action");
    let live = owner
        .find("registrar.activate()")
        .expect("owner publishes its live session");
    let initialize = owner
        .find("initialize_input_owner")
        .expect("owner activates device sources only after publication");
    assert!(register < live && live < initialize);

    let registration = source
        .split_once("fn prepare_input_irq_action(")
        .unwrap()
        .1
        .split_once("fn initialize_input_owner(")
        .unwrap()
        .0;
    assert!(registration.contains("register_shared_disabled"));

    let initialize = source
        .split_once("fn initialize_input_owner(")
        .unwrap()
        .1
        .split_once("fn input_irq_action(")
        .unwrap()
        .0;
    let action_enable = initialize.find("action.enable()?").unwrap();
    let source_enable = initialize.find("device.enable_irq()").unwrap();
    assert!(action_enable < source_enable);
}

#[test]
fn runtime_publishes_only_snapshot_and_event_facade() {
    let source = runtime_input_source();
    assert!(source.contains("InputDeviceSnapshot"));
    assert!(source.contains("InputEventPublisher"));
    assert!(source.contains("InputDeviceFacade"));
    assert!(!source.contains("ax_input::ErasedInputDevice"));
}
