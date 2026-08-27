use core::ptr::NonNull;

use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::{RuntimeError, RuntimeResult};

pub(crate) enum MappingTransactionError {
    NotStarted(RuntimeError),
    StateUncertain(RuntimeError),
}

pub(crate) fn map_dma_coherent_alias(
    paddr: PhysAddr,
    size: usize,
) -> Result<NonNull<u8>, MappingTransactionError> {
    let mut kernel_aspace = ax_mm::kernel_aspace().lock();
    map_alias_transaction(
        || {
            kernel_aspace
                .map_dma_coherent_alias(paddr, size)
                .map_err(Into::into)
        },
        |alias| {
            let alias_vaddr = VirtAddr::from_usize(alias.as_ptr() as usize);
            ax_hal::cache::flush_tlb_range_all_cpus(alias_vaddr, size)?;
            Ok(())
        },
    )
}

pub(crate) fn unmap_dma_coherent_alias(alias: NonNull<u8>, size: usize) -> RuntimeResult {
    // Keep the address-space lock across the shootdown so another allocation
    // cannot reuse this VA while any CPU may retain its old translation.
    let mut kernel_aspace = ax_mm::kernel_aspace().lock();
    update_mapping_transaction(
        || {
            kernel_aspace.unmap_dma_coherent_alias(alias, size)?;
            Ok(())
        },
        || {
            let alias_vaddr = VirtAddr::from_usize(alias.as_ptr() as usize);
            ax_hal::cache::flush_tlb_range_all_cpus(alias_vaddr, size)?;
            Ok(())
        },
    )
}

fn map_alias_transaction(
    map: impl FnOnce() -> RuntimeResult<NonNull<u8>>,
    shootdown: impl FnOnce(NonNull<u8>) -> RuntimeResult,
) -> Result<NonNull<u8>, MappingTransactionError> {
    let alias = map().map_err(MappingTransactionError::NotStarted)?;
    shootdown(alias).map_err(MappingTransactionError::StateUncertain)?;
    Ok(alias)
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
    fn mapping_transaction_updates_before_shootdown() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let update_events = events.clone();
        let shootdown_events = events.clone();

        let result = update_mapping_transaction(
            move || {
                update_events.borrow_mut().push("update");
                Ok(())
            },
            move || {
                shootdown_events.borrow_mut().push("shootdown");
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(*events.borrow(), vec!["update", "shootdown"]);
    }

    #[test]
    fn mapping_transaction_stops_when_update_fails() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let update_events = events.clone();
        let shootdown_events = events.clone();

        let result = update_mapping_transaction(
            move || {
                update_events.borrow_mut().push("update");
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
        assert_eq!(*events.borrow(), vec!["update"]);
    }

    #[test]
    fn mapping_transaction_propagates_shootdown_failure() {
        let result = update_mapping_transaction(
            || Ok(()),
            || Err(RuntimeError::from(TlbShootdownError::Timeout)),
        );

        assert_eq!(result, Err(RuntimeError::from(TlbShootdownError::Timeout)));
    }

    #[test]
    fn alias_mapping_distinguishes_not_started_from_uncertain_state() {
        let not_started = map_alias_transaction(
            || Err(RuntimeError::from(ax_mm::MmError::NoMemory)),
            |_| Ok(()),
        );
        assert!(matches!(
            not_started,
            Err(MappingTransactionError::NotStarted(RuntimeError::Mm(
                ax_mm::MmError::NoMemory
            )))
        ));

        let alias = NonNull::new(0x4000 as *mut u8).unwrap();
        let uncertain = map_alias_transaction(
            || Ok(alias),
            |_| Err(RuntimeError::from(TlbShootdownError::Timeout)),
        );
        assert!(matches!(
            uncertain,
            Err(MappingTransactionError::StateUncertain(
                RuntimeError::TlbShootdown(TlbShootdownError::Timeout)
            ))
        ));
    }
}
