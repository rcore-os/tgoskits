use crate::sync::context::{ContextBackend, enter_preempt_irqsave, exit_preempt_irqsave};

pub(super) const CONTEXT_RAW: u8 = 0;
pub(super) const CONTEXT_PREEMPT: u8 = 1;
pub(super) const CONTEXT_IRQSAVE: u8 = 2;
pub(super) const CONTEXT_PREEMPT_IRQSAVE: u8 = 3;

/// Runtime callbacks used by the external synchronization provider.
#[derive(Clone, Copy)]
pub struct ContextOperations {
    pub preempt_enter: fn() -> usize,
    pub preempt_exit: unsafe fn(usize),
    pub preempt_exit_irq_return: unsafe fn(usize),
    pub irq_save_and_disable: fn() -> usize,
    pub irq_restore: unsafe fn(usize),
    pub hardirq_enter: fn(),
    pub hardirq_exit: fn(),
}

struct ExternalContext<'ops>(&'ops ContextOperations);

impl ContextBackend for ExternalContext<'_> {
    type PreemptState = usize;
    type IrqState = usize;

    fn preempt_enter(&self) -> Self::PreemptState {
        (self.0.preempt_enter)()
    }

    fn preempt_exit(&self, state: Self::PreemptState) {
        // SAFETY: every caller passes a token returned by this backend.
        unsafe { (self.0.preempt_exit)(state) };
    }

    fn preempt_exit_irq_return(&self, state: Self::PreemptState) {
        // SAFETY: every caller passes the paired token while IRQs remain disabled.
        unsafe { (self.0.preempt_exit_irq_return)(state) };
    }

    fn irq_save_and_disable(&self) -> Self::IrqState {
        (self.0.irq_save_and_disable)()
    }

    fn irq_restore(&self, state: Self::IrqState) {
        // SAFETY: every caller passes the state returned by this backend.
        unsafe { (self.0.irq_restore)(state) };
    }
}

/// Opaque execution-context state returned to an external wrapper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ContextState {
    preempt: usize,
    irq: usize,
}

impl ContextState {
    pub const fn new(preempt: usize, irq: usize) -> Self {
        Self { preempt, irq }
    }

    pub const fn preempt(self) -> usize {
        self.preempt
    }

    pub const fn irq(self) -> usize {
        self.irq
    }
}

pub fn context_enter(context: u8, operations: &ContextOperations) -> ContextState {
    let backend = ExternalContext(operations);
    match context {
        CONTEXT_RAW => ContextState::new(0, 0),
        CONTEXT_PREEMPT => ContextState::new(backend.preempt_enter(), 0),
        CONTEXT_IRQSAVE => ContextState::new(0, backend.irq_save_and_disable()),
        CONTEXT_PREEMPT_IRQSAVE => {
            let (preempt, irq) = enter_preempt_irqsave(&backend);
            ContextState::new(preempt, irq)
        }
        _ => panic!("unknown external lock context {context}"),
    }
}

pub fn context_exit(context: u8, state: ContextState, operations: &ContextOperations) {
    let backend = ExternalContext(operations);
    match context {
        CONTEXT_RAW => {}
        CONTEXT_PREEMPT => backend.preempt_exit(state.preempt),
        CONTEXT_IRQSAVE => backend.irq_restore(state.irq),
        CONTEXT_PREEMPT_IRQSAVE => {
            exit_preempt_irqsave((state.preempt, state.irq), &backend);
        }
        _ => panic!("unknown external lock context {context}"),
    }
}

pub fn irq_return_preempt_enter(operations: &ContextOperations) -> usize {
    ExternalContext(operations).preempt_enter()
}

/// # Safety
///
/// `state` must come from [`irq_return_preempt_enter`] on the same execution
/// context and remain nested inside the active raw IRQ-save guard.
pub unsafe fn irq_return_preempt_exit(state: usize, operations: &ContextOperations) {
    ExternalContext(operations).preempt_exit_irq_return(state);
}

pub fn hardirq_enter(operations: &ContextOperations) {
    (operations.hardirq_enter)();
}

pub fn hardirq_exit(operations: &ContextOperations) {
    (operations.hardirq_exit)();
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use std::{sync::Mutex, vec::Vec};

    use super::*;

    const PREEMPT_TOKEN: usize = 0x1111;
    const IRQ_TOKEN: usize = 0x2222;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Event {
        PreemptEnter,
        IrqDisable,
        IrqRestore(usize),
        PreemptExit(usize),
    }

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());

    fn record(event: Event) {
        EVENTS.lock().unwrap().push(event);
    }

    fn preempt_enter() -> usize {
        record(Event::PreemptEnter);
        PREEMPT_TOKEN
    }

    unsafe fn preempt_exit(state: usize) {
        record(Event::PreemptExit(state));
    }

    unsafe fn preempt_exit_irq_return(_state: usize) {}

    fn irq_save_and_disable() -> usize {
        record(Event::IrqDisable);
        IRQ_TOKEN
    }

    unsafe fn irq_restore(state: usize) {
        record(Event::IrqRestore(state));
    }

    fn no_op() {}

    fn operations() -> ContextOperations {
        ContextOperations {
            preempt_enter,
            preempt_exit,
            preempt_exit_irq_return,
            irq_save_and_disable,
            irq_restore,
            hardirq_enter: no_op,
            hardirq_exit: no_op,
        }
    }

    fn take_events() -> Vec<Event> {
        core::mem::take(&mut *EVENTS.lock().unwrap())
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn combined_bridge_preserves_tokens_and_restores_irq_before_preempt() {
        let _serial = TEST_LOCK.lock().unwrap();
        take_events();

        let state = context_enter(CONTEXT_PREEMPT_IRQSAVE, &operations());
        assert_eq!(state, ContextState::new(PREEMPT_TOKEN, IRQ_TOKEN));
        assert_eq!(take_events(), [Event::PreemptEnter, Event::IrqDisable]);

        context_exit(CONTEXT_PREEMPT_IRQSAVE, state, &operations());
        assert_eq!(
            take_events(),
            [
                Event::IrqRestore(IRQ_TOKEN),
                Event::PreemptExit(PREEMPT_TOKEN)
            ]
        );
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn raw_bridge_does_not_invoke_callbacks() {
        let _serial = TEST_LOCK.lock().unwrap();
        take_events();

        let state = context_enter(CONTEXT_RAW, &operations());
        assert_eq!(state, ContextState::new(0, 0));
        context_exit(CONTEXT_RAW, state, &operations());

        assert!(take_events().is_empty());
    }
}
