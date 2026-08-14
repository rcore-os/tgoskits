use ax_hal::paging::MappingFlags;
use ax_memory_addr::VirtAddr;

use crate::RuntimeResult;

pub(crate) fn protect_kernel_range(
    start: VirtAddr,
    size: usize,
    flags: MappingFlags,
) -> RuntimeResult {
    // A shootdown error happens after the PTE update. Callers must treat any
    // associated storage as quarantined: rollback would itself require the
    // cross-CPU synchronization that just failed.
    update_mapping_transaction(
        || {
            let mut kernel_aspace = ax_mm::kernel_aspace().lock();
            kernel_aspace.protect(start, size, flags)?;
            Ok(())
        },
        || {
            ax_hal::cache::flush_tlb_range_all_cpus(start, size)?;
            Ok(())
        },
    )
}

fn update_mapping_transaction(
    protect: impl FnOnce() -> RuntimeResult,
    shootdown: impl FnOnce() -> RuntimeResult,
) -> RuntimeResult {
    protect()?;
    shootdown()
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec, vec::Vec};
    use core::cell::RefCell;

    use ax_hal::cache::TlbShootdownError;

    use super::*;
    use crate::RuntimeError;

    #[test]
    fn mapping_transaction_protects_before_shootdown() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let protect_events = events.clone();
        let shootdown_events = events.clone();

        let result = update_mapping_transaction(
            move || {
                protect_events.borrow_mut().push("protect");
                Ok(())
            },
            move || {
                shootdown_events.borrow_mut().push("shootdown");
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(*events.borrow(), vec!["protect", "shootdown"]);
    }

    #[test]
    fn mapping_transaction_stops_when_protect_fails() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let protect_events = events.clone();
        let shootdown_events = events.clone();

        let result = update_mapping_transaction(
            move || {
                protect_events.borrow_mut().push("protect");
                Err(RuntimeError::from(ax_mm::MmError::BadState("test")))
            },
            move || {
                shootdown_events.borrow_mut().push("shootdown");
                Ok(())
            },
        );

        assert_eq!(
            result,
            Err(RuntimeError::from(ax_mm::MmError::BadState("test")))
        );
        assert_eq!(*events.borrow(), vec!["protect"]);
    }

    #[test]
    fn mapping_transaction_propagates_shootdown_failure() {
        let result = update_mapping_transaction(
            || Ok(()),
            || Err(RuntimeError::from(TlbShootdownError::Timeout)),
        );

        assert_eq!(result, Err(RuntimeError::from(TlbShootdownError::Timeout)));
    }
}
