use std::{fs, path::Path};

#[test]
fn non_irq_guest_exit_restores_the_callers_complete_daif_state() {
    let source = read_vcpu_source();
    let run = section(&source, "    pub fn run(", "    /// Binds this vCPU");

    assert!(
        run.contains("HostIrqState::save_and_mask()"),
        "guest entry must save DAIF before masking host IRQs"
    );
    assert!(
        run.contains("TrapKind::Irq")
            && run.contains("self.pending_host_irq_state = Some(host_irq_state)")
            && run.contains("host_irq_state.restore()"),
        "only a lower-EL IRQ exit may retain the caller's masked DAIF state"
    );
    assert!(
        !run.contains("msr daifclr, #2"),
        "an outer IRQ-disabled caller must never be unconditionally enabled"
    );
    assert_in_order(
        run,
        &[
            "HostIrqState::save_and_mask()",
            "self.run_guest()",
            "TrapKind::Irq",
            "self.pending_host_irq_state = Some(host_irq_state)",
            "host_irq_state.restore()",
        ],
    );

    assert!(
        source.contains("mrs {saved_daif}, daif")
            && source.contains("msr daifset, #2")
            && source.contains("msr daif, {saved_daif}"),
        "AArch64 must follow save-DAIF, mask-I, restore-saved-DAIF semantics"
    );
}

#[test]
fn lower_el_irq_ownership_is_finished_once_after_backend_unbind() {
    let source = read_vcpu_source();
    let finish = section(
        &source,
        "    pub fn finish_post_unbind(",
        "    /// Sets a general-purpose register",
    );

    assert!(
        source.contains("pending_host_irq_state: Option<HostIrqState>"),
        "the saved DAIF owner must stay inside the vCPU until post-unbind completion"
    );
    assert_in_order(
        finish,
        &[
            "self.pending_host_irq_state.take()",
            "H::handle_post_unbind_host_irq(cpu_pin)",
            "host_irq_state.restore()",
        ],
    );
    assert!(
        !source.contains("fetch_pending_host_irq"),
        "a lower-EL exit must not fabricate a vector or claim the controller before unbind"
    );
}

#[test]
fn live_backend_operations_require_a_cpu_pin() {
    let source = read_vcpu_source();
    for operation in ["run", "bind", "unbind"] {
        let signature = section(
            &source,
            &format!("    pub fn {operation}"),
            if operation == "unbind" {
                "    /// Sets a general-purpose register"
            } else if operation == "bind" {
                "    /// Unbinds this vCPU"
            } else {
                "    /// Binds this vCPU"
            },
        );
        assert!(
            signature.contains("&CpuPin"),
            "ArmVcpu::{operation} must require a borrowed CPU pin"
        );
    }
}

fn read_vcpu_source() -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vcpu.rs"))
        .expect("vCPU source must remain readable")
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .expect("run function must remain present");
    let tail = &source[start..];
    let end = tail
        .find(end)
        .expect("run function boundary must remain present");
    &tail[..end]
}

fn assert_in_order(source: &str, patterns: &[&str]) {
    let mut cursor = 0;
    for pattern in patterns {
        let offset = source[cursor..]
            .find(pattern)
            .unwrap_or_else(|| panic!("missing ordered pattern {pattern:?}"));
        cursor += offset + pattern.len();
    }
}
