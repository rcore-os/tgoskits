#[test]
fn display_irq_and_control_are_owned_by_one_maintenance_thread() {
    let runtime = include_str!("../src/display.rs");
    let display = include_str!("../../axdisplay/src/lib.rs");
    let card0 = include_str!("../../../../StarryOS/kernel/src/pseudofs/dev/card0.rs");
    let rdif = include_str!("../../../../../drivers/interface/rdif-display/src/interface.rs");

    assert!(runtime.contains("spawn_maintenance_domain"));
    assert!(runtime.contains("registrar.register_shared_disabled"));
    assert!(runtime.contains("MaintenanceIrqAction"));
    assert!(!runtime.contains("Registration::register_shared_disabled_on"));
    assert!(runtime.contains("LocalIrqWake"));
    assert!(runtime.contains("DisplayMaintenanceEvent"));
    assert!(runtime.contains("DisplayIrqFallback"));
    assert!(runtime.contains("record_from_irq"));
    assert!(runtime.contains("take_line_quench_owner"));
    assert!(runtime.contains("action.release_quench()"));

    assert!(rdif.contains("DisplayExecution"));
    assert!(rdif.contains("take_irq_endpoint"));
    assert!(!rdif.contains("fn handle_irq"));

    assert!(!display.contains("MAIN_DISPLAY.lock()"));
    assert!(!display.contains("framebuffer_handle_irq"));
    assert!(!display.contains("framebuffer_enable_irq"));
    assert!(!display.contains("framebuffer_disable_irq"));

    assert!(!card0.contains("fn register_irq"));
    assert!(!card0.contains("framebuffer_handle_irq"));
    assert!(!card0.contains("IrqRequest::new"));
    assert!(!card0.contains("irq_handle:"));
}

#[test]
fn display_driver_is_not_remotely_called_through_a_global_lock() {
    let display = include_str!("../../axdisplay/src/lib.rs");
    let device = include_str!("../../axdisplay/src/device.rs");

    assert!(display.contains("DisplayFlushService"));
    assert!(!display.contains("SpinMutex<ErasedDisplayDevice>"));
    assert!(!device.contains("fn handle_irq"));
    assert!(!device.contains("fn enable_irq"));
    assert!(!device.contains("fn disable_irq"));
}
