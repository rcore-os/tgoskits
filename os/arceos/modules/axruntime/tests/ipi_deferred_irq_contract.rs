//! Source-level contract for deferred IPI callback execution.

const AXIPI: &str = include_str!("../../axipi/src/lib.rs");
const AXRUNTIME_GUARD: &str = include_str!("../src/guard.rs");

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("source section start must exist");
    let end = source[start..]
        .find(end)
        .map(|offset| start + offset)
        .expect("source section end must exist");
    &source[start..end]
}

#[test]
fn hard_irq_ipi_entry_only_publishes_deferred_work() {
    let handler = source_section(
        AXIPI,
        "pub fn ipi_handler()",
        "pub fn drain_deferred_callbacks()",
    );

    assert!(handler.contains("mark_deferred_pending"));
    assert!(!handler.contains("pop_one"));
    assert!(!handler.contains("callback.call()"));
    assert!(!handler.contains("Box"));
}

#[test]
fn irq_return_safe_point_owns_bounded_callback_execution() {
    let drain = source_section(AXIPI, "pub fn drain_deferred_callbacks()", "fn cpu_index");
    assert!(drain.contains("DEFERRED_CALLBACK_BATCH"));
    assert!(drain.contains("callback.call()"));
    assert!(drain.contains("request_follow_up_ipi"));

    let irq_return = source_section(
        AXRUNTIME_GUARD,
        "unsafe fn preempt_exit_irq_return()",
        "fn current_thread_id()",
    );
    assert!(irq_return.contains("ax_ipi::drain_deferred_callbacks()"));
    assert!(irq_return.contains("exit_lock_preempt(true)"));
}
