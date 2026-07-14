use std::{fs, path::Path};

#[test]
fn guest_run_restores_the_callers_complete_daif_state() {
    let source = read_vcpu_source();
    let run = section(&source, "    pub fn run(", "    /// Binds this vCPU");

    assert!(
        run.contains("HostIrqState::save_and_mask()"),
        "guest entry must save DAIF before masking host IRQs"
    );
    assert!(
        run.contains("host_irq_state.restore()"),
        "guest exit must restore the caller's DAIF state"
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
