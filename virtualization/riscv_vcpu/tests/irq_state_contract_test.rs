use std::{fs, path::Path};

#[test]
fn guest_run_returns_a_cpu_pinned_raii_exit() {
    let source = read_vcpu_source();
    let run = section(&source, "    pub fn run<'cpu>(", "    /// Binds the vCPU");

    assert!(
        run.contains("HostIrqState::save_and_disable()"),
        "guest entry must atomically save and mask the caller's IRQ state"
    );
    assert!(
        !run.contains("restore") && !run.contains("set_sie"),
        "guest run must return to the bound owner before restoring host IRQ delivery"
    );
    assert!(run.contains("RiscvVcpuResult<RiscvBoundExit<'cpu>>"));
    assert!(run.contains("RiscvBoundExit::new(exit, host_irq_state)"));
    assert!(
        !run.contains("sstatus::set_sie()"),
        "an outer IRQ-disabled caller must never be unconditionally enabled"
    );
    assert_in_order(
        run,
        &[
            "HostIrqState::save_and_disable()",
            "_run_guest(&mut self.regs)",
            "self.vmexit_handler()",
            "RiscvBoundExit::new(exit, host_irq_state)",
        ],
    );

    assert!(
        source.contains("csrrc {saved_sstatus}, sstatus, {sie_mask}"),
        "saving SSTATUS.SIE and disabling it must be one atomic CSR operation"
    );
    assert!(
        source.contains("saved_sie: sie::read().bits()"),
        "guest entry must preserve the host's per-source SIE mask"
    );
    assert!(
        source.contains("impl Drop for HostIrqState")
            && source.contains("sie::write(sie::Sie::from_bits(self.saved_sie))"),
        "the private IRQ token must restore every host SIE source on finish or error unwind"
    );
    assert!(
        source.contains("self.saved_sstatus & SSTATUS_SIE"),
        "nested restore must enable SIE only when the saved outer state allowed it"
    );
    assert!(
        source.contains("pub struct RiscvBoundExit<'cpu>")
            && source.contains("_cpu_pin: PhantomData<&'cpu CpuPin>")
            && source.contains("pub const fn event(&self) -> RiscvVmExit")
            && source.contains("impl Drop for RiscvBoundExit<'_>"),
        "the bound exit must borrow the CPU pin and expose only a Copy event view"
    );
    assert!(
        !source.contains("host_irq_state: Option<HostIrqState>")
            && !source.contains("pub fn finish_bound_exit"),
        "IRQ restoration ownership must never be hidden inside the reusable vCPU"
    );
}

#[test]
fn live_csr_operations_require_a_cpu_pin() {
    let source = read_vcpu_source();
    for operation in ["run", "bind", "unbind"] {
        let declaration = if operation == "run" {
            "pub fn run<'cpu>(".to_owned()
        } else {
            format!("pub fn {operation}(")
        };
        let signature = source
            .split_once(&declaration)
            .unwrap_or_else(|| panic!("missing RiscvVcpu::{operation}"))
            .1
            .split_once('{')
            .expect("vCPU operation must have a body")
            .0;
        assert!(
            signature.contains("CpuPin"),
            "RiscvVcpu::{operation} must require a borrowed CPU pin"
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
        .unwrap_or_else(|| panic!("source section start {start:?} must remain present"));
    let tail = &source[start..];
    let end = tail
        .find(end)
        .unwrap_or_else(|| panic!("source section end {end:?} must remain present"));
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
