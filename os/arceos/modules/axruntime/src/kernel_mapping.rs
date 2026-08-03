use ax_hal::{cache::TlbShootdownError, paging::MappingFlags};
use ax_memory_addr::VirtAddr;
use axklib::{AxError, AxResult};

pub(crate) fn protect_kernel_range(start: VirtAddr, size: usize, flags: MappingFlags) -> AxResult {
    // A shootdown error happens after the PTE update. Callers must treat any
    // associated storage as quarantined: rollback would itself require the
    // cross-CPU synchronization that just failed.
    update_mapping_transaction(
        || {
            let mut kernel_aspace = ax_mm::kernel_aspace().lock();
            kernel_aspace.protect(start, size, flags)
        },
        || ax_hal::cache::flush_tlb_range_all_cpus(start, size).map_err(map_shootdown_error),
    )
}

fn update_mapping_transaction(
    protect: impl FnOnce() -> AxResult,
    shootdown: impl FnOnce() -> AxResult,
) -> AxResult {
    protect()?;
    shootdown()
}

fn map_shootdown_error(err: TlbShootdownError) -> AxError {
    match err {
        TlbShootdownError::CpuOffline | TlbShootdownError::Unsupported => AxError::Unsupported,
        TlbShootdownError::Timeout => AxError::TimedOut,
        TlbShootdownError::Platform => AxError::Io,
    }
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec, vec::Vec};
    use core::cell::RefCell;

    use super::*;

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
                Err(AxError::BadState)
            },
            move || {
                shootdown_events.borrow_mut().push("shootdown");
                Ok(())
            },
        );

        assert_eq!(result, Err(AxError::BadState));
        assert_eq!(*events.borrow(), vec!["protect"]);
    }

    #[test]
    fn mapping_transaction_propagates_shootdown_failure() {
        let result = update_mapping_transaction(|| Ok(()), || Err(AxError::TimedOut));

        assert_eq!(result, Err(AxError::TimedOut));
    }
}
