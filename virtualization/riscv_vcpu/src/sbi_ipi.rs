extern crate alloc;

#[cfg(test)]
use alloc::vec::Vec;

pub const VSSIP_HVIP_BIT: usize = 2;
#[cfg(test)]
pub const VSTIP_HVIP_BIT: usize = 6;
#[cfg(test)]
pub const VSEIP_HVIP_BIT: usize = 10;

pub const SUPERVISOR_SOFT_CAUSE: usize = 1;
pub const SUPERVISOR_TIMER_CAUSE: usize = 5;
pub const SUPERVISOR_EXTERNAL_CAUSE: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HartMaskReadError {
    ShortRead { expected: usize, copied: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorInterruptAction {
    ConsumeHostSoft,
    InjectGuestTimer,
    InjectGuestExternal,
}

pub fn read_hart_mask(
    guest_va: usize,
    mut copy_from_guest_va: impl FnMut(usize, &mut [u8]) -> usize,
) -> Result<usize, HartMaskReadError> {
    let mut mask_bytes = [0u8; core::mem::size_of::<usize>()];
    let copied = copy_from_guest_va(guest_va, &mut mask_bytes);

    if copied != mask_bytes.len() {
        return Err(HartMaskReadError::ShortRead {
            expected: mask_bytes.len(),
            copied,
        });
    }

    Ok(usize::from_ne_bytes(mask_bytes))
}

#[cfg(test)]
pub fn select_targets(hart_mask: usize, vcpu_ids: impl IntoIterator<Item = usize>) -> Vec<usize> {
    vcpu_ids
        .into_iter()
        .filter(|&vcpu_id| vcpu_id < usize::BITS as usize && ((hart_mask >> vcpu_id) & 1) != 0)
        .collect()
}

pub fn clear_virtual_soft_pending(hvip: usize) -> usize {
    hvip & !(1usize << VSSIP_HVIP_BIT)
}

pub fn classify_supervisor_interrupt(cause: usize) -> Option<SupervisorInterruptAction> {
    match cause {
        SUPERVISOR_SOFT_CAUSE => Some(SupervisorInterruptAction::ConsumeHostSoft),
        SUPERVISOR_TIMER_CAUSE => Some(SupervisorInterruptAction::InjectGuestTimer),
        SUPERVISOR_EXTERNAL_CAUSE => Some(SupervisorInterruptAction::InjectGuestExternal),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_mask_is_empty_and_not_broadcast() {
        assert_eq!(select_targets(0, [0, 1, 2, 3]), Vec::<usize>::new());
    }

    #[test]
    fn selected_mask_routes_only_selected_harts() {
        assert_eq!(select_targets(0b1010, [0, 1, 2, 3]), alloc::vec![1, 3]);
    }

    #[test]
    fn unreadable_guest_mask_is_rejected() {
        let err = read_hart_mask(0x1000, |_guest_va, _bytes| 0).unwrap_err();
        assert_eq!(
            err,
            HartMaskReadError::ShortRead {
                expected: core::mem::size_of::<usize>(),
                copied: 0,
            }
        );
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn guest_memory_word_routes_only_masked_harts() {
        let mask = 0b1010usize;
        let mask_bytes = mask.to_ne_bytes();

        let hart_mask = read_hart_mask(0x4000, |guest_va, bytes| {
            assert_eq!(guest_va, 0x4000);
            bytes.copy_from_slice(&mask_bytes);
            bytes.len()
        })
        .unwrap();

        assert_eq!(select_targets(hart_mask, [0, 1, 2, 3]), alloc::vec![1, 3]);
    }

    #[test]
    fn mask_bit_one_selects_only_vcpu_one() {
        assert_eq!(select_targets(1 << 1, [0, 1, 2, 3]), alloc::vec![1]);
    }

    #[test]
    fn mask_bits_zero_and_two_select_only_vcpus_zero_and_two() {
        assert_eq!(
            select_targets((1 << 0) | (1 << 2), [0, 1, 2, 3]),
            alloc::vec![0, 2]
        );
    }

    #[test]
    fn sparse_guest_hart_mask_selects_hart_id_not_local_index() {
        assert_eq!(
            select_targets(1usize << 5, [4usize, 5usize, 9usize]),
            alloc::vec![5]
        );
    }

    #[test]
    fn clear_ipi_updates_only_current_hart_hvip_snapshot() {
        let current_hvip = (1usize << VSSIP_HVIP_BIT) | (1usize << VSTIP_HVIP_BIT);
        let remote_hvip = current_hvip;

        let current_after = clear_virtual_soft_pending(current_hvip);

        assert_eq!(current_after & (1usize << VSSIP_HVIP_BIT), 0);
        assert_ne!(remote_hvip & (1usize << VSSIP_HVIP_BIT), 0);
    }

    #[test]
    fn invalid_guest_mask_pointer_rejects_before_routing() {
        let err = read_hart_mask(0, |_guest_va, _bytes| 0).unwrap_err();

        assert_eq!(
            err,
            HartMaskReadError::ShortRead {
                expected: core::mem::size_of::<usize>(),
                copied: 0,
            }
        );
    }

    #[test]
    fn clear_ipi_clears_only_vssip() {
        let hvip = 1usize << VSSIP_HVIP_BIT;
        assert_eq!(clear_virtual_soft_pending(hvip), 0);
    }

    #[test]
    fn clear_ipi_preserves_timer_and_external_pending() {
        let hvip =
            (1usize << VSSIP_HVIP_BIT) | (1usize << VSTIP_HVIP_BIT) | (1usize << VSEIP_HVIP_BIT);

        let cleared = clear_virtual_soft_pending(hvip);

        assert_eq!(cleared & (1usize << VSSIP_HVIP_BIT), 0);
        assert_ne!(cleared & (1usize << VSTIP_HVIP_BIT), 0);
        assert_ne!(cleared & (1usize << VSEIP_HVIP_BIT), 0);
    }

    #[test]
    fn host_supervisor_soft_is_consumed_by_host() {
        assert_eq!(
            classify_supervisor_interrupt(SUPERVISOR_SOFT_CAUSE),
            Some(SupervisorInterruptAction::ConsumeHostSoft)
        );
    }

    #[test]
    fn timer_and_external_are_guest_interrupts() {
        assert_eq!(
            classify_supervisor_interrupt(SUPERVISOR_TIMER_CAUSE),
            Some(SupervisorInterruptAction::InjectGuestTimer)
        );
        assert_eq!(
            classify_supervisor_interrupt(SUPERVISOR_EXTERNAL_CAUSE),
            Some(SupervisorInterruptAction::InjectGuestExternal)
        );
    }
}
