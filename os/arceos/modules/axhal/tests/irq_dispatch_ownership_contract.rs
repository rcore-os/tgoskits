//! IRQ-dispatch ownership contracts shared by CPU traps and VM exits.

const AX_CPU_TRAP: &str = include_str!("../../../../../components/axcpu/src/trap.rs");
const AX_HAL: &str = include_str!("../src/lib.rs");
const AX_HAL_IRQ: &str = include_str!("../src/irq.rs");
const AXVM_X86_64: &str = include_str!("../../../../../virtualization/axvm/src/arch/x86_64/mod.rs");
const AXVM_AARCH64: &str =
    include_str!("../../../../../virtualization/axvm/src/arch/aarch64/mod.rs");
const AXVM_AARCH64_GIC: &str =
    include_str!("../../../../../virtualization/axvm/src/arch/aarch64/gic.rs");
const AXVM_RISCV64: &str =
    include_str!("../../../../../virtualization/axvm/src/arch/riscv64/mod.rs");
const AXVM_LOONGARCH64: &str =
    include_str!("../../../../../virtualization/axvm/src/arch/loongarch64/mod.rs");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DispatchOwner {
    TrapReturn,
    TaskOrVmExit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreemptCompletion {
    FinishTrapReturn,
    DropNormally,
}

fn completion_for(owner: DispatchOwner, _irqs_enabled_after_handler: bool) -> PreemptCompletion {
    match owner {
        DispatchOwner::TrapReturn => PreemptCompletion::FinishTrapReturn,
        DispatchOwner::TaskOrVmExit => PreemptCompletion::DropNormally,
    }
}

fn source_section<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing source section start: {start}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing source section end: {end}"))
        .0
}

#[test]
fn handler_irq_mask_cannot_change_the_owner_selected_completion() {
    for owner in [DispatchOwner::TrapReturn, DispatchOwner::TaskOrVmExit] {
        assert_eq!(
            completion_for(owner, false),
            completion_for(owner, true),
            "the entry owner, not a callback's live IRQ mask, selects the return path",
        );
    }
}

#[test]
fn trap_return_ownership_is_a_typed_linear_capability() {
    assert!(
        AX_CPU_TRAP.contains("pub struct TrapIrqPermit"),
        "ax-cpu must mint an unforgeable trap-return permit at its architecture entry",
    );
    assert!(
        AX_CPU_TRAP.contains("fn(TrapIrqPermit) -> bool"),
        "the installed IRQ hook must consume trap-return ownership explicitly",
    );
    assert!(
        AX_CPU_TRAP.contains("vector: usize")
            && AX_CPU_TRAP.contains("pub const fn vector(&self) -> usize"),
        "the permit must bind the architecture vector so a handler cannot pair it with another",
    );
    assert!(
        !AX_HAL.contains("breakpoint_handler, dispatch_irq, dispatch_page_fault"),
        "ax-hal must not re-export the bare trap dispatcher to task or VM-exit callers",
    );
}

#[test]
fn trap_and_task_dispatch_have_distinct_completion_apis() {
    assert!(
        AX_HAL_IRQ.contains("pub fn handle_trap_irq("),
        "the CPU trap hook needs an entry point that consumes TrapIrqPermit",
    );
    assert!(
        AX_HAL_IRQ.contains("pub fn handle_irq_from_task("),
        "deferred and VM-exit work needs an ordinary task-context dispatcher",
    );
    assert!(
        !AX_HAL_IRQ.contains("if crate::asm::irqs_enabled()"),
        "a handler's final IRQ mask must not select drop versus finish_irq_return",
    );

    let task_dispatch = source_section(
        AX_HAL_IRQ,
        "pub fn handle_irq_from_task(",
        "/// Installs the default",
    );
    let irq_restore = task_dispatch
        .find("drop(irq_guard);")
        .expect("task dispatch must restore its saved IRQ state");
    let restored_assertion = task_dispatch
        .find("task IRQ guard failed to restore the enabled host IRQ state")
        .expect("deferred dispatch must prove that the backend restored host IRQs");
    let preempt_exit = task_dispatch
        .find("drop(preempt_guard);")
        .expect("task dispatch must use ordinary preemption exit");
    assert!(
        task_dispatch.contains("task IRQ dispatch entered before the host IRQ state was restored")
            && irq_restore < restored_assertion
            && restored_assertion < preempt_exit,
        "deferred dispatch must enter enabled and restore IRQs before ordinary preempt-exit",
    );
}

#[test]
fn post_unbind_dispatch_is_a_pinned_masked_linear_capability() {
    assert!(
        AX_HAL_IRQ.contains("pub struct PinnedHostIrqPermit<'pin>"),
        "post-unbind host IRQ ownership must be represented by a borrowed CPU pin",
    );
    let dispatch = source_section(
        AX_HAL_IRQ,
        "pub fn handle_pinned_host_irq(",
        "/// Claims and dispatches a pending IRQ from ordinary task",
    );
    assert!(
        dispatch.contains("assert_trap_irqs_masked")
            && dispatch.contains("prepare_irq_context")
            && dispatch.contains("handle(TrapVector"),
        "the post-unbind owner must claim and dispatch while DAIF remains masked",
    );
    for forbidden in ["IrqGuard::new", "finish_irq_return", "enable_irqs"] {
        assert!(
            !dispatch.contains(forbidden),
            "post-unbind dispatch must leave IRQ restoration to the saved DAIF owner: {forbidden}",
        );
    }
}

#[test]
fn axvm_external_interrupts_do_not_call_the_trap_only_entry() {
    for (architecture, source) in [
        ("x86_64", AXVM_X86_64),
        ("aarch64", AXVM_AARCH64),
        ("aarch64-gic", AXVM_AARCH64_GIC),
        ("loongarch64", AXVM_LOONGARCH64),
    ] {
        assert!(
            !source.contains("ax_hal::irq::handle_irq("),
            "{architecture} VM-exit code must use the typed task/pinned dispatcher",
        );
    }

    assert!(
        !AXVM_RISCV64.contains("ax_hal::irq::handle_irq("),
        "RISC-V must retain its bound, generation-bearing PLIC claim path",
    );
}

#[test]
fn aarch64_current_and_lower_el_irqs_have_distinct_single_owners() {
    assert!(!AXVM_AARCH64.contains("fetch_pending_host_irq"));
    assert!(!AXVM_AARCH64.contains("Some(0)"));
    assert_eq!(
        AXVM_AARCH64.matches("handle_pinned_host_irq(").count(),
        1,
        "lower-EL IRQ work must claim and dispatch exactly once in the post-unbind owner",
    );
    assert_eq!(
        AXVM_AARCH64.matches("handle_trap_irq(").count(),
        1,
        "the independent current-EL exception path must consume one trap permit",
    );
    assert!(
        !AXVM_AARCH64_GIC.contains("handle_irq_from_task(")
            && !AXVM_AARCH64_GIC.contains("handle_trap_irq("),
        "the GIC helper must not hide a second generic dispatch owner",
    );
}
