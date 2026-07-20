//! Deterministic contract for user IRQ return and the runtime baton.

const RUNTIME_GUARD: &str = include_str!("../src/guard.rs");
const RUNTIME_TASK: &str = include_str!("../src/task.rs");
const HAL_IRQ: &str = include_str!("../../axhal/src/irq.rs");
const USER_SPACE_COMMON: &str =
    include_str!("../../../../../components/axcpu/src/uspace_common.rs");
const STARRY_USER_TASK: &str = include_str!("../../../../StarryOS/kernel/src/task/user.rs");
const USER_CONTEXTS: [(&str, &str); 4] = [
    (
        "x86_64",
        include_str!("../../../../../components/axcpu/src/x86_64/uspace.rs"),
    ),
    (
        "aarch64",
        include_str!("../../../../../components/axcpu/src/aarch64/uspace.rs"),
    ),
    (
        "riscv64",
        include_str!("../../../../../components/axcpu/src/riscv/uspace.rs"),
    ),
    (
        "loongarch64",
        include_str!("../../../../../components/axcpu/src/loongarch64/uspace.rs"),
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerBaton {
    Active,
    Transferred,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IrqReturnState {
    preempt_depth: u32,
    baton: SchedulerBaton,
}

impl IrqReturnState {
    const fn task_context() -> Self {
        Self {
            preempt_depth: 0,
            baton: SchedulerBaton::Finished,
        }
    }

    fn enter_preempt_guard(&mut self) {
        assert_eq!(self.baton, SchedulerBaton::Finished);
        self.preempt_depth += 1;
    }

    fn finish_irq_return(&mut self, need_resched: bool) {
        assert_eq!(self.preempt_depth, 1);
        assert_eq!(self.baton, SchedulerBaton::Finished);
        self.preempt_depth = 0;
        if need_resched {
            self.baton = SchedulerBaton::Active;
        }
    }

    fn transfer_switch_baton(&mut self) {
        assert_eq!(self.preempt_depth, 0);
        assert_eq!(self.baton, SchedulerBaton::Active);
        self.baton = SchedulerBaton::Transferred;
    }

    fn resume_irq_return(&mut self) {
        assert_eq!(self.preempt_depth, 0);
        assert_eq!(self.baton, SchedulerBaton::Transferred);
        self.baton = SchedulerBaton::Finished;
    }

    fn assert_user_return_safe(self) {
        assert_eq!(self, Self::task_context());
    }

    fn is_user_return_safe(self) -> bool {
        self == Self::task_context()
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

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing function body: {signature}"));

    let mut depth = 0usize;
    for (offset, character) in source[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body: {signature}")
}

#[test]
fn irq_return_without_reschedule_consumes_the_only_preempt_depth() {
    let mut state = IrqReturnState::task_context();

    state.enter_preempt_guard();
    state.finish_irq_return(false);

    state.assert_user_return_safe();
}

#[test]
fn irq_return_reschedule_transfers_and_finishes_the_only_preempt_depth() {
    let mut state = IrqReturnState::task_context();

    state.enter_preempt_guard();
    state.finish_irq_return(true);
    assert_eq!(state.preempt_depth, 0);
    assert_eq!(state.baton, SchedulerBaton::Active);

    state.transfer_switch_baton();
    state.resume_irq_return();

    state.assert_user_return_safe();
}

#[test]
fn ordinary_preempt_guard_cannot_cross_the_user_return_boundary() {
    let mut state = IrqReturnState::task_context();
    state.enter_preempt_guard();

    assert!(
        !state.is_user_return_safe(),
        "an ordinary guard must be released or transferred before UserContext returns",
    );

    let boundary = source_section(
        RUNTIME_GUARD,
        "fn validate_user_context_boundary(",
        "fn schedule_context_snapshot()",
    );
    assert!(boundary.contains("boundary.accepts(snapshot)"));
    assert!(RUNTIME_GUARD.contains("self.preempt_lock_depth == 0"));
}

#[test]
fn production_irq_return_uses_the_same_two_typed_transitions() {
    let trap_handler = source_section(
        HAL_IRQ,
        "pub fn handle_trap_irq(",
        "/// Claims and dispatches",
    );
    assert!(trap_handler.contains("let guard = PreemptGuard::new();"));
    assert!(trap_handler.contains("guard.finish_irq_return();"));
    assert!(!trap_handler.contains("drop(guard);"));

    let task_handler = source_section(
        HAL_IRQ,
        "pub fn handle_irq_from_task(",
        "/// Installs the default",
    );
    assert!(task_handler.contains("let preempt_guard = PreemptGuard::new();"));
    assert!(task_handler.contains("let irq_guard = IrqGuard::new();"));
    assert!(task_handler.contains("drop(irq_guard);"));
    assert!(task_handler.contains("drop(preempt_guard);"));
    assert!(!task_handler.contains("finish_irq_return"));

    let preempt_exit = source_section(
        RUNTIME_GUARD,
        "fn exit_lock_preempt(origin: PreemptExitOrigin)",
        "pub(crate) fn enter_scheduler_frame_guard",
    );
    assert!(preempt_exit.contains("irq_owner.scheduler_entry()"));
    assert!(preempt_exit.contains("state.exit_lock_preempt();"));

    let irq_owner = source_section(
        RUNTIME_GUARD,
        "impl PreemptExitIrqOwner",
        "struct ScheduleContextSnapshot",
    );
    assert!(irq_owner.contains("PreemptExitOrigin::IrqReturn"));
    assert!(irq_owner.contains("RuntimeSchedulerEntry::IrqReturn"));
    assert!(irq_owner.contains("restore_saved_irq_state(self)"));

    let claim = source_section(
        RUNTIME_GUARD,
        "fn claim_preempt_exit_scheduler(&mut self) -> bool",
        "fn transfer_scheduler_baton(&mut self)",
    );
    assert!(
        claim.contains("self.lock_depth = 0;"),
        "IrqReturn scheduling must consume, not retain, the ordinary guard depth",
    );
}

#[test]
fn runtime_dispatches_raw_user_irq_after_kernel_accounting_and_before_irq_restore() {
    let runtime_run = function_body(RUNTIME_TASK, "pub fn run_user_context(");
    let raw_return = runtime_run
        .find("context.run_raw()")
        .expect("the architecture boundary must return an undispatched raw user exit");
    let kernel_accounting = runtime_run
        .find("transition_irqoff(UserExecutionState::Kernel")
        .expect("kernel accounting must be published before decoding the raw exit");
    let decode = runtime_run
        .find("context.decode_raw_exit(raw_exit)")
        .expect("the runtime must decode the architecture exit after kernel accounting");
    let dispatch = runtime_run
        .find("interrupt.dispatch()")
        .expect("ax-runtime must consume and dispatch a raw user IRQ");
    let task_return_validation = runtime_run[dispatch..]
        .find("UserContextBoundary::TaskReturn")
        .map(|offset| dispatch + offset)
        .expect("the runtime must validate task context after IRQ dispatch");
    let enable_irqs = runtime_run
        .rfind("enable_irqs")
        .expect("the runtime must reopen IRQs only after dispatch and validation");

    assert!(
        raw_return < kernel_accounting
            && kernel_accounting < decode
            && decode < dispatch
            && dispatch < task_return_validation
            && task_return_validation < enable_irqs,
        "the owned return sequence must be raw exit -> kernel accounting -> decode -> raw IRQ \
         dispatch -> task-boundary validation -> IRQ restore",
    );
}

#[test]
fn raw_interrupt_is_a_typed_architecture_to_runtime_capability() {
    assert!(USER_SPACE_COMMON.contains("pub struct RawUserInterrupt"));
    assert!(USER_SPACE_COMMON.contains("pub struct RawUserExit"));
    assert!(USER_SPACE_COMMON.contains("pub enum DecodedUserExit"));
    assert!(USER_SPACE_COMMON.contains("Interrupt(RawUserInterrupt)"));
    assert!(USER_SPACE_COMMON.contains("Reason(UserExitReason)"));
    assert!(USER_SPACE_COMMON.contains("pub enum UserExitReason"));
    assert!(USER_SPACE_COMMON.contains("PhantomData<*mut ()>"));
    assert!(!USER_SPACE_COMMON.contains("impl Clone for RawUserExit"));
    assert!(!USER_SPACE_COMMON.contains("impl Copy for RawUserExit"));
    assert!(USER_SPACE_COMMON.contains("pub fn dispatch(self) -> bool"));
    assert!(!USER_SPACE_COMMON.contains("pub const fn dispatch_token"));
    assert!(
        !USER_SPACE_COMMON.contains("pub enum ReturnReason"),
        "the old ambiguous return type must not bypass raw-interrupt ownership",
    );
}

fn assert_architecture_user_context_returns_raw_irq_masked(architecture: &str, source: &str) {
    let architecture_run = function_body(source, "pub fn run_raw(");
    assert!(
        architecture_run.contains("-> RawUserExit"),
        "{architecture} must expose the undispatched exit through the typed raw boundary",
    );
    assert!(
        architecture_run.contains("RawUserExit") && !architecture_run.contains("DecodedUserExit"),
        "{architecture} run_raw must return only the opaque context-bound token",
    );
    assert!(
        !architecture_run.contains("dispatch_irq("),
        "{architecture} run_raw must not dispatch before kernel accounting is published",
    );
    for forbidden_decode in [
        "Cr2::read_raw",
        "scause::read",
        "stval::read",
        "estat::read",
        "ESR_EL1",
        "FAR_EL1",
    ] {
        assert!(
            !architecture_run.contains(forbidden_decode),
            "{architecture} run_raw decoded {forbidden_decode} before kernel accounting",
        );
    }
    assert!(
        !architecture_run.contains("crate::asm::enable_irqs();"),
        "{architecture} must return from the raw user exception window with IRQs masked; \
         otherwise an unconsumed ordinary PreemptGuard becomes observable before the runtime \
         validates the user-return boundary",
    );

    let decode = function_body(source, "pub fn decode_raw_exit(");
    assert!(decode.contains("-> DecodedUserExit"));
    assert!(decode.contains("assert_bound_to(self)"));
}

#[test]
fn x86_64_user_context_returns_with_raw_irqs_masked() {
    assert_architecture_user_context_returns_raw_irq_masked(USER_CONTEXTS[0].0, USER_CONTEXTS[0].1);
}

#[test]
fn aarch64_user_context_returns_with_raw_irqs_masked() {
    assert_architecture_user_context_returns_raw_irq_masked(USER_CONTEXTS[1].0, USER_CONTEXTS[1].1);
}

#[test]
fn riscv64_user_context_returns_with_raw_irqs_masked() {
    assert_architecture_user_context_returns_raw_irq_masked(USER_CONTEXTS[2].0, USER_CONTEXTS[2].1);
}

#[test]
fn loongarch64_user_context_returns_with_raw_irqs_masked() {
    assert_architecture_user_context_returns_raw_irq_masked(USER_CONTEXTS[3].0, USER_CONTEXTS[3].1);
}

#[test]
fn starry_only_observes_already_dispatched_user_exit_reasons() {
    assert!(STARRY_USER_TASK.contains("UserExitReason"));
    assert!(!STARRY_USER_TASK.contains("RawUserExit"));
    assert!(!STARRY_USER_TASK.contains("RawUserInterrupt"));
    assert!(!STARRY_USER_TASK.contains("ReturnReason"));
}
