//! Cross-layer contract for one-shot timer ownership at IRQ return.

const RUNTIME: &str = include_str!("../src/lib.rs");
const TASK_RUNTIME: &str = include_str!("../src/task.rs");
const GUARD: &str = include_str!("../src/guard.rs");
const HAL_IRQ: &str = include_str!("../../axhal/src/irq.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start: {start}"));
    tail.split_once(end)
        .unwrap_or_else(|| panic!("missing section end: {end}"))
        .0
}

#[test]
fn timer_irq_claims_the_armed_event_before_accounting_or_reprogramming() {
    let handler = section(RUNTIME, "fn timer_irq_handler(", "fn ipi_irq_handler(");
    let claim = handler
        .find("claim_timer_interrupt();")
        .expect("timer IRQ must claim the armed one-shot first");
    let accounting = handler
        .find("task::on_timer_irq(scheduler_tick);")
        .expect("timer IRQ must publish bounded scheduler accounting");
    let reprogram = handler
        .find("program_next_timer();")
        .expect("timer IRQ must keep the periodic clockevent live");

    assert!(claim < accounting && accounting < reprogram);
}

#[test]
fn armed_and_claimed_hardware_events_have_one_typed_mux_owner() {
    assert!(RUNTIME.contains("enum TimerArmState"));
    assert!(RUNTIME.contains("TimerArmState::Disarmed"));
    assert!(RUNTIME.contains("TimerArmState::Armed"));
    let program = section(RUNTIME, "fn program_next_timer()", "fn timer_irq_handler(");
    assert!(
        program.contains("timer_mux.next_programming(deadline)"),
        "the mux, not scattered raw slots, must linearize one-shot programming"
    );
}

#[test]
fn task_deadline_publication_reaches_the_mux_before_any_safe_point_return() {
    let publication = section(
        TASK_RUNTIME,
        "fn program_oneshot_timer(deadline_ns: u64)",
        "fn dispatch_expired_timer(",
    );
    let desired = publication
        .find("NEXT_TASK_TIMER_DEADLINE_NS.write_current_raw(deadline_ns)")
        .expect("ax-task must first publish the desired deadline");
    let mux = publication
        .find("crate::program_next_timer();")
        .expect("the runtime mux must commit the desired deadline");
    assert!(desired < mux);
}

#[test]
fn controller_eoi_and_hard_irq_exit_precede_scheduler_entry() {
    let dispatch = section(
        HAL_IRQ,
        "pub fn handle_trap_irq(",
        "/// Claims and dispatches",
    );
    let handled = dispatch
        .find("let handled = handle(TrapVector(vector)).is_some();")
        .expect("IRQ framework dispatch must own controller completion");
    let safe_point = dispatch
        .find("guard.finish_irq_return();")
        .expect("trap return must enter the scheduler only after dispatch returns");
    assert!(handled < safe_point);

    let runtime_return = section(
        GUARD,
        "unsafe fn preempt_exit_irq_return()",
        "fn current_thread_id()",
    );
    let callbacks = runtime_return
        .find("ax_ipi::drain_deferred_callbacks();")
        .expect("bounded deferred callbacks belong to IRQ return");
    let scheduler = runtime_return
        .find("exit_lock_preempt(true);")
        .expect("the final preempt depth must transfer directly to the scheduler");
    assert!(callbacks < scheduler);
}
