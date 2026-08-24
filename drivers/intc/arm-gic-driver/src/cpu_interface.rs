//! Ordering helpers for GIC CPU-interface completion operations.

/// Performs a CPU-interface completion write before its synchronization step.
///
/// The closures keep the ordering contract testable on non-AArch64 hosts while
/// allowing the production path to inline the architectural register write and
/// barrier without introducing a dynamic call boundary.
#[inline(always)]
pub(crate) fn write_completion(write_register: impl FnOnce(), synchronize: impl FnOnce()) {
    write_register();
    synchronize();
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{cell::RefCell, vec::Vec};

    use super::write_completion;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum CompletionStep {
        RegisterWrite,
        Synchronize,
    }

    #[test]
    fn cpu_interface_completion_write_is_synchronized_before_return() {
        let steps = RefCell::new(Vec::new());

        write_completion(
            || steps.borrow_mut().push(CompletionStep::RegisterWrite),
            || steps.borrow_mut().push(CompletionStep::Synchronize),
        );

        assert_eq!(
            steps.into_inner(),
            [CompletionStep::RegisterWrite, CompletionStep::Synchronize]
        );
    }
}
