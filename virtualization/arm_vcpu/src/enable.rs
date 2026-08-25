//! Ordering contract for publishing AArch64 EL2 virtualization state.

pub(crate) trait El2EnableOps {
    fn install_exception_vector(&mut self);

    fn synchronize_context(&mut self);

    fn enable_virtualization(&mut self);

    fn install_current_el_irq_handler(&mut self);
}

pub(crate) fn enable_el2(ops: &mut impl El2EnableOps) {
    ops.install_current_el_irq_handler();
    ops.install_exception_vector();
    ops.synchronize_context();
    ops.enable_virtualization();
    ops.synchronize_context();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum EnableStep {
        InstallIrqHandler,
        InstallExceptionVector,
        SynchronizeContext,
        EnableVirtualization,
    }

    struct EnableRecorder {
        steps: [Option<EnableStep>; 5],
        len: usize,
    }

    impl EnableRecorder {
        const fn new() -> Self {
            Self {
                steps: [None; 5],
                len: 0,
            }
        }

        fn push(&mut self, step: EnableStep) {
            self.steps[self.len] = Some(step);
            self.len += 1;
        }
    }

    impl El2EnableOps for EnableRecorder {
        fn install_exception_vector(&mut self) {
            self.push(EnableStep::InstallExceptionVector);
        }

        fn synchronize_context(&mut self) {
            self.push(EnableStep::SynchronizeContext);
        }

        fn enable_virtualization(&mut self) {
            self.push(EnableStep::EnableVirtualization);
        }

        fn install_current_el_irq_handler(&mut self) {
            self.push(EnableStep::InstallIrqHandler);
        }
    }

    #[test]
    fn publishes_handler_then_vbar_isb_then_hcr_isb() {
        let mut recorder = EnableRecorder::new();

        enable_el2(&mut recorder);

        assert_eq!(
            &recorder.steps[..recorder.len],
            [
                Some(EnableStep::InstallIrqHandler),
                Some(EnableStep::InstallExceptionVector),
                Some(EnableStep::SynchronizeContext),
                Some(EnableStep::EnableVirtualization),
                Some(EnableStep::SynchronizeContext),
            ]
        );
    }
}
