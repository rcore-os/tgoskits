//! SBI supervisor-software-interrupt routing over the fixed hart topology.
//!
//! Target resolution and delivery are separated so that resolution happens
//! while the hardware backend is still loaded, and delivery happens unbound
//! through the run-bound signal target.

use std::vec::Vec;

use crate::{
    InterruptTriggerMode,
    irq::model::{PendingVcpuInterrupt, VirtualInterruptId},
};

/// `scause` cause number of the supervisor software interrupt.
///
/// The architecture injection path adds the interrupt bit when building the
/// complete `scause` value.
pub(crate) const SUPERVISOR_SOFTWARE_INTERRUPT_ID: VirtualInterruptId = VirtualInterruptId(1);

/// Resolves one SBI hart mask into the target VM-local vCPU ids.
///
/// A `hart_mask_base` of [`usize::MAX`] selects every planned vCPU. Any hart
/// that is absent from the fixed topology, any overflowing hart id, and any two
/// mask bits that resolve to the same vCPU reject the complete request before a
/// single target is published.
pub(crate) fn resolve_targets(
    hart_mask: usize,
    hart_mask_base: usize,
    all_vcpu_ids: impl FnOnce() -> Vec<usize>,
    mut resolve_vcpu_id: impl FnMut(usize) -> Option<usize>,
) -> Result<Vec<usize>, IpiTargetError> {
    if hart_mask_base == usize::MAX {
        return Ok(all_vcpu_ids());
    }

    let mut targets = Vec::new();
    for bit in 0..usize::BITS {
        if hart_mask & (1usize << bit) == 0 {
            continue;
        }
        let hart_id = hart_mask_base
            .checked_add(bit as usize)
            .ok_or(IpiTargetError::HartIdOverflow)?;
        let target_vcpu_id =
            resolve_vcpu_id(hart_id).ok_or(IpiTargetError::UnavailableHart(hart_id))?;
        if targets.contains(&target_vcpu_id) {
            return Err(IpiTargetError::DuplicateVcpu(target_vcpu_id));
        }
        targets.push(target_vcpu_id);
    }
    Ok(targets)
}

/// Publishes the supervisor software interrupt to every resolved target.
///
/// `publish` runs for each target in mask order and must publish the pending
/// state before it kicks. A failure keeps the already published prefix and is
/// reported as an SBI failure.
pub(crate) fn deliver<E>(
    targets: &[usize],
    mut publish: impl FnMut(usize, PendingVcpuInterrupt) -> Result<(), E>,
) -> Result<(), IpiDeliveryError<E>> {
    let interrupt = PendingVcpuInterrupt {
        id: SUPERVISOR_SOFTWARE_INTERRUPT_ID,
        trigger: InterruptTriggerMode::LevelTriggered,
        source: None,
    };

    for &target_vcpu_id in targets {
        publish(target_vcpu_id, interrupt).map_err(|source| IpiDeliveryError {
            target_vcpu_id,
            source,
        })?;
    }
    Ok(())
}

/// Delivery failure of one already resolved SBI IPI request.
#[derive(Debug)]
pub(crate) struct IpiDeliveryError<E> {
    /// The vCPU whose publication failed.
    pub(crate) target_vcpu_id: usize,
    /// The underlying wake failure.
    pub(crate) source: E,
}

/// Topology rejection of one SBI hart mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IpiTargetError {
    /// A hart id derived from the mask overflowed the address width.
    HartIdOverflow,
    /// The guest selected a hart that the fixed topology does not contain.
    UnavailableHart(usize),
    /// Two selected harts resolve to the same vCPU.
    DuplicateVcpu(usize),
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use std::vec::Vec;

    use ax_plat::irq::{IrqError, RiscvHvIrqIf};

    use super::*;

    /// Keeps target userspace tests independent from a live dynamic platform.
    struct TestRiscvHvIrqIf;

    #[ax_plat::impl_plat_interface]
    impl RiscvHvIrqIf for TestRiscvHvIrqIf {
        fn activate_guest_plic_source(_source: u32, _target_cpu: usize) -> Result<(), IrqError> {
            Err(IrqError::Unsupported)
        }

        fn deactivate_guest_plic_source(_source: u32) -> Result<(), IrqError> {
            Err(IrqError::Unsupported)
        }

        fn complete_guest_plic_source(_source: u32) -> bool {
            false
        }
    }

    fn interrupt() -> PendingVcpuInterrupt {
        PendingVcpuInterrupt {
            id: SUPERVISOR_SOFTWARE_INTERRUPT_ID,
            trigger: InterruptTriggerMode::LevelTriggered,
            source: None,
        }
    }

    #[test]
    fn selected_harts_publish_level_vssip_in_mask_order() {
        let mut published = Vec::new();
        let targets = resolve_targets(0b101, 4, Vec::new, |hart_id| match hart_id {
            4 => Some(2),
            6 => Some(0),
            _ => None,
        })
        .unwrap();

        deliver(&targets, |target_vcpu_id, interrupt| {
            published.push((target_vcpu_id, interrupt));
            Ok::<_, ()>(())
        })
        .unwrap();

        assert_eq!(published, [(2, interrupt()), (0, interrupt())]);
    }

    #[test]
    fn broadcast_selects_every_available_vcpu() {
        let targets = resolve_targets(
            0,
            usize::MAX,
            || std::vec![2, 0, 1],
            |_| panic!("broadcast must not resolve individual hart IDs"),
        )
        .unwrap();

        assert_eq!(targets, [2, 0, 1]);
    }

    #[test]
    fn empty_mask_resolves_no_target() {
        let targets = resolve_targets(
            0,
            0,
            || panic!("ordinary empty mask must not enumerate all vCPUs"),
            |_| panic!("empty mask must not resolve a hart ID"),
        )
        .unwrap();

        assert!(targets.is_empty());
    }

    #[test]
    fn unavailable_hart_rejects_the_whole_request_before_publication() {
        let error =
            resolve_targets(0b11, 4, Vec::new, |hart_id| (hart_id == 4).then_some(2)).unwrap_err();

        assert_eq!(error, IpiTargetError::UnavailableHart(5));
    }

    #[test]
    fn overflowing_hart_id_rejects_the_whole_request() {
        let error = resolve_targets(1 << 2, usize::MAX - 1, Vec::new, |_| Some(0)).unwrap_err();

        assert_eq!(error, IpiTargetError::HartIdOverflow);
    }

    #[test]
    fn duplicate_vcpu_mapping_rejects_the_whole_request() {
        let error = resolve_targets(0b11, 4, Vec::new, |_| Some(2)).unwrap_err();

        assert_eq!(error, IpiTargetError::DuplicateVcpu(2));
    }

    #[test]
    fn delivery_failure_keeps_the_published_prefix_and_reports_the_target() {
        let mut published = Vec::new();
        let error = deliver(&[4, 5, 6], |target_vcpu_id, _interrupt| {
            published.push(target_vcpu_id);
            if target_vcpu_id == 5 {
                Err("queue closed")
            } else {
                Ok(())
            }
        })
        .unwrap_err();

        assert_eq!(error.target_vcpu_id, 5);
        assert_eq!(error.source, "queue closed");
        assert_eq!(published, [4, 5]);
    }
}
