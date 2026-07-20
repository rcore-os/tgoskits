//! Source contracts for CPU-pinned accelerator IRQ maintenance owners.

const DEV: &str = include_str!("../src/pseudofs/dev/mod.rs");
const KPU: &str = include_str!("../src/pseudofs/dev/kpu.rs");
const TPU: &str = include_str!("../src/pseudofs/dev/tpu/device.rs");

#[test]
fn accelerator_irq_registration_uses_runtime_linear_owner() {
    assert!(!DEV.contains("struct IrqRegistration"));
    assert!(!DEV.contains("impl Drop for IrqRegistration"));
    assert!(!DEV.contains("let _ = ax_runtime::hal::irq::free_irq"));
    assert!(!DEV.contains("request_shared_disabled"));
}

#[test]
fn kpu_irq_is_registered_and_serviced_by_one_pinned_owner() {
    assert!(KPU.contains("spawn_maintenance_domain::<KpuMaintenanceEvent"));
    assert!(KPU.contains("registrar.register_shared_disabled"));
    assert!(!KPU.contains("Registration::register_shared_disabled_on"));
    assert!(KPU.contains("LocalIrqWake<KpuMaintenanceEvent>"));
    assert!(KPU.contains("registration.close()"));
    assert!(!KPU.contains("IrqWaitCell"));
    assert!(!KPU.contains("IrqWaitRegistration"));
    assert!(!KPU.contains("Box::leak"));
    assert!(!KPU.contains("request_shared_disabled"));
}

#[test]
fn tpu_hardware_worker_owns_irq_registration_and_local_wake() {
    assert!(TPU.contains("spawn_maintenance_domain::<TpuMaintenanceEvent"));
    assert!(TPU.contains("registrar.register_shared_disabled"));
    assert!(!TPU.contains("Registration::register_shared_disabled_on"));
    assert!(TPU.contains("LocalIrqWake<TpuMaintenanceEvent>"));
    assert!(TPU.contains("registration.close()"));
    assert!(!TPU.contains("IrqWaitCell"));
    assert!(!TPU.contains("IrqWaitRegistration"));
    assert!(!TPU.contains("Box::leak"));
    assert!(!TPU.contains("request_shared_disabled"));
}

#[test]
fn hard_irq_callbacks_only_capture_publish_and_request_irq_return_wake() {
    assert!(KPU.contains("publish_from_irq"));
    assert!(TPU.contains("publish_from_irq"));
    assert!(KPU.contains("IrqReturn::Wake"));
    assert!(TPU.contains("IrqReturn::Wake"));
    assert!(!KPU.contains("KPU_DONE_WQ.notify_all();\n    ax_runtime::hal::irq::IrqReturn"));
    assert!(!TPU.contains("DONE_WQ.notify_all();\n        ax_runtime::hal::irq::IrqReturn"));
}

#[test]
fn kpu_completion_wait_never_discovers_success_by_reading_device_status() {
    let wait = function_body(KPU, "    fn wait_done(");

    assert!(wait.contains("KPU_IRQ_COUNT"));
    assert!(wait.contains("wait_timeout_until"));
    assert!(!wait.contains("KpuOperation::IsDone"));
    assert!(!wait.contains("is_done"));
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let tail = &source[start..];
    let open = tail.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in tail[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &tail[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}
