//! Source-level contract for Starry perf's scheduler-sensitive CPU-local data.

const SAMPLING: &str = include_str!("../src/perf/sampling.rs");

fn function_source(name: &str, next_name: &str) -> &'static str {
    let start = SAMPLING
        .find(name)
        .unwrap_or_else(|| panic!("missing function marker: {name}"));
    let rest = &SAMPLING[start..];
    let end = rest
        .find(next_name)
        .unwrap_or_else(|| panic!("missing next function marker: {next_name}"));
    &rest[..end]
}

#[test]
fn perf_registry_borrows_stay_inside_live_cpu_pins() {
    assert!(
        !SAMPLING.contains("current_ref_mut_raw()"),
        "perf sampling must not obtain an escaping raw current-CPU reference"
    );

    for function in [
        function_source("pub fn register(", "pub fn unregister("),
        function_source("pub fn unregister(", "pub fn ensure_pmu_irq_registered("),
    ] {
        assert!(function.contains("let guard = PreemptIrqGuard::new();"));
        assert!(function.contains("REGISTRY.with_current_mut_raw(guard.cpu_pin(),"));
    }

    let handler = function_source("pub fn pmu_overflow_handler(", "fn build_sample(");
    let interrupted_pc = handler
        .find("let ip = ax_cpu::pmu::interrupted_pc();")
        .expect("handler must capture the interrupted PC");
    let irq_guard = handler
        .find("let irq_guard = IrqGuard::new();")
        .expect("hard-IRQ CPU-local access must own an IRQ guard");
    let pinned_borrow = handler
        .find("REGISTRY.with_current_mut_raw(irq_guard.cpu_pin(),")
        .expect("handler registry access must borrow the guard's CpuPin");

    assert!(
        interrupted_pc < irq_guard && irq_guard < pinned_borrow,
        "capture architectural IRQ state first, then pin the registry borrow"
    );
}
