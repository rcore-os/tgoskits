//! Ordering contract for publishing AArch64 EL2 virtualization state.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum El2EnableStep {
    InstallCurrentElIrqHandler,
    SynchronizeContext,
    EnableVirtualization,
}

pub(crate) const EL2_ENABLE_STEPS: [El2EnableStep; 4] = [
    El2EnableStep::InstallCurrentElIrqHandler,
    El2EnableStep::SynchronizeContext,
    El2EnableStep::EnableVirtualization,
    El2EnableStep::SynchronizeContext,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_handler_then_isb_then_hcr_isb() {
        assert_eq!(
            EL2_ENABLE_STEPS,
            [
                El2EnableStep::InstallCurrentElIrqHandler,
                El2EnableStep::SynchronizeContext,
                El2EnableStep::EnableVirtualization,
                El2EnableStep::SynchronizeContext,
            ]
        );
    }
}
