//! Deterministic contract for a preemption-guard exit that schedules and migrates.

const RUNTIME_GUARD: &str = include_str!("../src/guard.rs");
const TASK_FACADE: &str = include_str!("../../../../../components/ax-task/src/facade.rs");
const TASK_RUNTIME_ABI: &str = include_str!("../../../../../components/ax-task/src/runtime.rs");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerBaton {
    Active,
    Transferred,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CpuGuardState {
    preempt_depth: u32,
    baton: SchedulerBaton,
}

impl CpuGuardState {
    const fn task_context() -> Self {
        Self {
            preempt_depth: 0,
            baton: SchedulerBaton::Finished,
        }
    }

    fn enter_guard(&mut self) {
        assert_eq!(self.baton, SchedulerBaton::Finished);
        self.preempt_depth += 1;
    }

    fn claim_preempt_exit(&mut self) {
        assert_eq!(self.preempt_depth, 1);
        assert_eq!(self.baton, SchedulerBaton::Finished);
        self.preempt_depth = 0;
        self.baton = SchedulerBaton::Active;
    }

    fn transfer_to_switch_tail(&mut self) {
        assert_eq!(self.preempt_depth, 0);
        assert_eq!(self.baton, SchedulerBaton::Active);
        self.baton = SchedulerBaton::Transferred;
    }

    fn finish_resumed_frame(&mut self) {
        assert_eq!(self.preempt_depth, 0);
        assert_ne!(self.baton, SchedulerBaton::Finished);
        self.baton = SchedulerBaton::Finished;
    }

    fn assert_task_context(self) {
        assert_eq!(self, Self::task_context());
    }
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing source section start: {start}"));
    tail.split_once(end)
        .unwrap_or_else(|| panic!("missing source section end: {end}"))
        .0
}

#[test]
fn ordinary_guard_drop_consumes_depth_before_a_migrating_switch() {
    let mut departure_cpu = CpuGuardState::task_context();
    departure_cpu.enter_guard();
    departure_cpu.claim_preempt_exit();
    departure_cpu.transfer_to_switch_tail();

    // A different continuation finishes the departure CPU's switch baton.
    departure_cpu.finish_resumed_frame();
    departure_cpu.assert_task_context();

    // The original guard destructor later resumes on a CPU whose outgoing
    // continuation transferred that CPU's independent scheduler baton.
    let mut resumed_cpu = CpuGuardState {
        preempt_depth: 0,
        baton: SchedulerBaton::Transferred,
    };
    resumed_cpu.finish_resumed_frame();
    resumed_cpu.assert_task_context();
}

#[test]
fn preempt_exit_checks_the_resumed_cpu_postcondition_before_returning() {
    let branch = source_section(
        RUNTIME_GUARD,
        "if must_schedule {",
        "state.exit_lock_preempt();",
    );
    let schedule = branch
        .find("schedule_current_cpu_from_preempt_exit")
        .expect("the final guard exit must enter the typed scheduler frame");
    let postcondition = branch
        .find("assert_preempt_exit_completed")
        .expect("the resumed guard destructor must validate the CPU-local postcondition");

    assert!(
        schedule < postcondition,
        "the postcondition is meaningful only after the scheduler continuation resumes",
    );
}

#[test]
fn preempt_exit_keeps_irq_restore_owned_by_the_outer_guard() {
    let return_abi = source_section(
        TASK_RUNTIME_ABI,
        "pub enum RuntimeSchedulerReturn {",
        "/// Result of an operation that creates one opaque runtime resource.",
    );
    assert!(
        return_abi.contains("PreemptExit"),
        "preemption-guard continuations need a return kind distinct from ordinary task entry",
    );

    let return_mapping = source_section(TASK_FACADE, "let return_to = match entry {", "Ok(Self {");
    assert!(
        return_mapping.contains("RuntimeSchedulerReturn::PreemptExit"),
        "a PreemptExit scheduler frame must return to its outer guard with IRQs still masked",
    );

    let runtime_exit = source_section(
        RUNTIME_GUARD,
        "match return_to {",
        "/// Verifies the fixed CPU-local baton immediately before the raw switch.",
    );
    assert!(
        runtime_exit.contains("RuntimeSchedulerReturn::PreemptExit"),
        "the runtime must finish the baton without restoring IRQs for PreemptExit",
    );
}
