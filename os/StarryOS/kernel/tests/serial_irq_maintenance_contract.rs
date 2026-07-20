//! Contracts for one CPU-pinned serial IRQ maintenance owner.

const SERIAL: &str = include_str!("../src/pseudofs/dev/tty/serial.rs");

#[test]
fn serial_device_progress_belongs_to_one_maintenance_thread() {
    assert!(
        SERIAL.contains("spawn_maintenance_domain"),
        "each UART must create one explicitly CPU-pinned maintenance domain"
    );
    assert!(
        SERIAL.contains("MaintenanceRegistrar"),
        "the owner thread must register its own IRQ action"
    );
    assert!(SERIAL.contains("LocalOwnerCell::pin(core)"));
    assert!(SERIAL.contains("maintenance_thread: SpinNoIrq<Option<MaintenanceThread>>"));
    assert!(SERIAL.contains("install_maintenance_thread"));
    assert!(SERIAL.contains("local_owner_cell"));
    assert!(SERIAL.contains("task::pin_current_cpu()"));
    assert!(SERIAL.contains("registrar.register_shared_disabled"));
    assert!(SERIAL.contains("MaintenanceIrqAction"));
    assert!(SERIAL.contains("session.begin_close()"));
    assert!(SERIAL.contains("registration.synchronize()"));
    assert!(SERIAL.contains("session.finish_close()"));
    let owner_loop = function_body(SERIAL, "fn serial_owner_loop(");
    let fault = owner_loop
        .find("if fault")
        .expect("owner must classify faults");
    let completion = owner_loop
        .find("publish_serial_facts")
        .expect("owner must publish completion facts");
    let rearm = owner_loop
        .find("IrqSourceControl::rearm")
        .expect("owner must explicitly rearm contained sources");
    assert!(fault < completion && completion < rearm);
    assert!(owner_loop.contains("drain_owner(SERIAL_EVENT_BATCH_LIMIT"));
    for forbidden in [
        "WorkQueue::new",
        "queue_work_on(",
        "run_on_cpu_sync_raw",
        "run_on_owner",
        "spawn_serial_event_worker",
        "OwnerId,",
        "SerialPort",
        "IrqNotify",
    ] {
        assert!(
            !SERIAL.contains(forbidden),
            "serial state progression still uses forbidden path `{}`",
            forbidden
        );
    }
}

#[test]
fn hard_irq_only_captures_and_wakes_its_local_owner() {
    assert!(SERIAL.contains("capture_irq()"));
    assert!(SERIAL.contains("publish_from_irq"));
    assert!(SERIAL.contains("LocalIrqWake"));
    assert!(SERIAL.contains("IrqReturn::Wake"));
    assert!(SERIAL.contains("IrqReturn::DisableActionAndWake"));
    assert!(SERIAL.contains("IrqReturn::MaskLineAndWake"));
    assert!(SERIAL.contains("IrqEndpoint::contain"));
    for forbidden in [
        "IrqReturn::Defer",
        "IrqContinuation",
        "finish_irq_continuation",
        "ThreadWakeHandle::wake",
    ] {
        assert!(
            !SERIAL.contains(forbidden),
            "hard IRQ path still exposes forbidden continuation/wake symbol `{}`",
            forbidden
        );
    }
    let hard_irq = function_body(SERIAL, "fn serial_irq_action(");
    for forbidden in [
        "service_masked",
        "SerialSoftWork",
        "publish_serial_facts",
        "IrqSourceControl::rearm",
        "input_source.wake",
        "output_source.wake",
    ] {
        assert!(
            !hard_irq.contains(forbidden),
            "hard IRQ still advances serial state through `{}`",
            forbidden
        );
    }
}

#[test]
fn device_sources_are_armed_only_after_the_owner_action_is_live() {
    let startup = function_body(SERIAL, "fn start_serial_owner(");
    let enable = startup
        .find("registration.enable()")
        .expect("owner must enable its registered action");
    let activate = startup
        .find("SerialCore::activate_interrupts")
        .expect("owner must explicitly arm device sources");

    assert!(
        enable < activate,
        "device sources must remain masked until the OS action is enabled"
    );
    assert_ordered(
        startup,
        &[
            "registration.release_quench()",
            ".store(false, Ordering::Release)",
            "SerialCore::activate_interrupts",
        ],
    );
}

#[test]
fn uncontained_capture_fault_masks_the_line_without_retrying_device_containment() {
    let hard_irq = function_body(SERIAL, "fn serial_irq_action(");
    let branch = hard_irq
        .split("FaultContainment::Uncontained =>")
        .nth(1)
        .expect("hard IRQ must handle an uncontained driver fault");

    assert!(branch.contains("line_quenched.store(true, Ordering::Release)"));
    assert!(branch.contains("IrqReturn::MaskLineAndWake"));
    assert!(
        !branch.contains("contain_serial_irq("),
        "an uncontained source cannot be reclassified as action-local"
    );
}

#[test]
fn non_owner_tx_submission_only_uses_the_domain_mailbox() {
    let submit = function_body(SERIAL, "fn submit_tx(");

    assert!(submit.contains("publish_cause"));
    assert!(submit.contains("MaintenanceCauses::SUBMIT"));
    assert!(!submit.contains("capture_irq"));
    assert!(!submit.contains("service"));
    assert!(!submit.contains("SerialCore"));
}

#[test]
fn rx_rearm_ownership_is_retained_outside_the_bounded_event_mailbox() {
    let backend = function_body(SERIAL, "impl SerialBackend {");
    let drain = function_body(SERIAL, "    fn drain_rx(");

    assert!(SERIAL.contains("pending_rearm: SpinNoIrq<Option<MaskedSource>>"));
    assert!(drain.contains("merge_pending_rearm"));
    assert!(drain.contains("publish_cause(MaintenanceCauses::SUBMIT)"));
    assert!(backend.contains("take_pending_rearm"));
    assert!(
        !drain.contains("submit_request"),
        "a full event mailbox must not discard the unique RX rearm fact"
    );
}

#[test]
fn close_revokes_remote_admission_before_releasing_irq_ownership() {
    let close = function_body(SERIAL, "fn close_serial_maintenance(");
    let admission = close
        .find("close_serial_admission")
        .expect("close must revoke the public serial facade");
    let quiesce = close
        .find("quiesce_serial_irq")
        .expect("close must contain the device and disable its IRQ action");

    assert!(admission < quiesce);
    assert!(SERIAL.contains("self.maintenance.lock().take()"));
    assert!(SERIAL.contains("self.started.store(false, Ordering::Release)"));
}

#[test]
fn teardown_contains_the_device_before_reopening_or_destroying_the_irq_action() {
    let quiesce = function_body(SERIAL, "fn quiesce_serial_irq(");
    assert_ordered(
        quiesce,
        &[
            "SerialCore::shutdown",
            "registration.disable()",
            "registration.release_quench()",
            "registration.synchronize()",
        ],
    );

    let close = function_body(SERIAL, "fn close_serial_maintenance(");
    assert!(close.contains("quiesce_serial_irq"));
    assert!(
        close.contains("registration.close()"),
        "normal teardown must consume the registration instead of relying on Drop"
    );
    assert!(
        !close.contains("drop(registration)"),
        "a live callback must never be anonymously dropped during teardown"
    );
}

#[test]
fn public_backend_paths_never_borrow_the_owner_local_uart() {
    let backend = function_body(SERIAL, "impl SerialBackend {");

    assert!(backend.contains("self.tx.lock().submit"));
    assert!(backend.contains("self.rx.lock().drain"));
    assert!(backend.contains("self.pending_config.lock()"));
    assert!(backend.contains("self.pending_rearm.lock()"));
    assert!(backend.contains("publish_cause"));
    for forbidden in ["with_owner", "with_irq", "capture_irq", "service_masked"] {
        assert!(
            !backend.contains(forbidden),
            "a non-owner serial backend path still accesses UART state through `{}`",
            forbidden
        );
    }
}

#[test]
fn emergency_output_never_waits_for_or_accesses_the_remote_owner() {
    let emergency = function_body(SERIAL, "fn emergency_write_without_owner(");

    assert!(emergency.contains("emergency_write_outcome"));
    for forbidden in [
        "wait(",
        "wait_until",
        "with_owner",
        "with_irq",
        "publish_cause",
        "submit_request",
        "SerialCore",
    ] {
        assert!(
            !emergency.contains(forbidden),
            "emergency output still crosses the owner boundary through `{}`",
            forbidden
        );
    }
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{}`", signature));
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
    panic!("unterminated function `{}`", signature)
}

fn assert_ordered(source: &str, needles: &[&str]) {
    let mut cursor = 0;
    for needle in needles {
        let offset = source[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing ordered step `{needle}`"));
        cursor += offset + needle.len();
    }
}
