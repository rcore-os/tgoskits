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

    let helper = function_source("unsafe fn with_registry_mut", "/// Registers `slot`");
    assert!(helper.contains("ax_percpu::with_cpu_pin(|pin|"));
    assert!(helper.contains("ax_percpu::with_exclusive_cpu(pin, |exclusive|"));
    assert!(helper.contains("REGISTRY.with_current_mut(exclusive, operation)"));

    for function in [
        function_source("pub fn register(", "pub fn unregister("),
        function_source("pub fn unregister(", "pub fn ensure_pmu_irq_registered("),
    ] {
        let guard = function
            .find("let _guard = NoPreemptIrqSave::new();")
            .expect("process-context registry mutation must disable preemption and local IRQs");
        let pinned_borrow = function
            .find("with_registry_mut(")
            .expect("registry mutation must use the scoped CPU capability helper");
        assert!(guard < pinned_borrow);
    }

    let handler = function_source("pub fn pmu_overflow_handler(", "fn build_sample(");
    let interrupted_pc = handler
        .find("let ip = ax_cpu::pmu::interrupted_pc();")
        .expect("handler must capture the interrupted PC");
    let pinned_borrow = handler
        .find("with_registry_mut(")
        .expect("hard-IRQ registry access must use the scoped CPU capability helper");

    assert!(
        interrupted_pc < pinned_borrow,
        "capture architectural IRQ state before borrowing the pinned registry"
    );
}
