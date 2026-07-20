use std::{fs, path::PathBuf};

fn interface_source() -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/interface.rs"))
        .expect("read rdif-input interface")
}

#[test]
fn interrupt_execution_requires_typed_endpoint_and_fallible_control() {
    let source = interface_source();

    assert!(source.contains("pub enum InputExecution"));
    assert!(source.contains("pub type InputIrqEndpoint"));
    assert!(source.contains("fn execution(&self) -> InputExecution"));
    assert!(source.contains("fn enable_irq(&mut self) -> Result<(), InputError>;"));
    assert!(source.contains("fn disable_irq(&mut self) -> Result<(), InputError>;"));
    assert!(source.contains("fn take_irq_endpoint(&mut self) -> Option<InputIrqEndpoint>"));
    assert!(source.contains("fn rearm_irq("));
    assert!(
        !source.contains("fn enable_irq(&mut self) {}")
            && !source.contains("fn disable_irq(&mut self) {}"),
        "interrupt source control must never silently succeed"
    );
}
