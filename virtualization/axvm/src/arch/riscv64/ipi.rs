//! RISC-V guest IPI routing through the VM runtime interrupt channel.

use std::vec::Vec;

use riscv_vcpu::{RiscvIpiCompletion, RiscvIpiRequest};

use super::{AxvmRiscvVcpu, RiscvDeferredRunWork};
use crate::{
    AxVMRef, AxVmResult, InterruptTriggerMode,
    architecture::{BoundVcpuExit, cpu_up::VmArchCpuIdResolver},
    irq::{
        model::{PendingVcpuInterrupt, VirtualInterruptId},
        sender::VmInterruptSender,
    },
    vm::AxVCpuRef,
};

const SUPERVISOR_SOFTWARE_INTERRUPT_ID: VirtualInterruptId = VirtualInterruptId(1);

pub(super) fn handle(
    vm: &AxVMRef,
    vcpu: &AxVCpuRef<AxvmRiscvVcpu>,
    request: RiscvIpiRequest,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let sender = VmInterruptSender::new(vm);
    let completion = match route_hart_mask(
        request.hart_mask(),
        request.hart_mask_base(),
        || vm.vcpu_list().iter().map(|vcpu| vcpu.id()).collect(),
        |hart_id| vm.vcpu_id_for_arch_cpu_id(hart_id),
        |target_vcpu_id, interrupt| sender.send(target_vcpu_id, interrupt),
    ) {
        Ok(()) => RiscvIpiCompletion::Success,
        Err(error) => {
            match &error {
                IpiRouteError::InvalidTarget(source) => warn!(
                    "VM[{}] VCpu[{}] rejected SBI IPI request {:?}: {:?}",
                    vm.id(),
                    vcpu.id(),
                    request,
                    source
                ),
                IpiRouteError::Delivery {
                    target_vcpu_id,
                    source,
                } => warn!(
                    "VM[{}] failed to deliver SBI IPI to VCpu[{}]: {:?}",
                    vm.id(),
                    target_vcpu_id,
                    source
                ),
            }
            error.completion()
        }
    };
    vcpu.get_arch_vcpu().complete_ipi(request, completion);
    Ok(BoundVcpuExit::Continue)
}

fn route_hart_mask<E>(
    hart_mask: usize,
    hart_mask_base: usize,
    all_vcpu_ids: impl FnOnce() -> Vec<usize>,
    resolve_vcpu_id: impl FnMut(usize) -> Option<usize>,
    publish: impl FnMut(usize, PendingVcpuInterrupt) -> Result<(), E>,
) -> Result<(), IpiRouteError<E>> {
    let targets = resolve_targets(hart_mask, hart_mask_base, all_vcpu_ids, resolve_vcpu_id)
        .map_err(IpiRouteError::InvalidTarget)?;
    deliver(&targets, publish)
}

fn deliver<E>(
    targets: &[usize],
    mut publish: impl FnMut(usize, PendingVcpuInterrupt) -> Result<(), E>,
) -> Result<(), IpiRouteError<E>> {
    let interrupt = PendingVcpuInterrupt {
        id: SUPERVISOR_SOFTWARE_INTERRUPT_ID,
        trigger: InterruptTriggerMode::LevelTriggered,
    };

    for &target_vcpu_id in targets {
        publish(target_vcpu_id, interrupt).map_err(|source| IpiRouteError::Delivery {
            target_vcpu_id,
            source,
        })?;
    }
    Ok(())
}

fn resolve_targets(
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

#[derive(Debug)]
enum IpiRouteError<E> {
    InvalidTarget(IpiTargetError),
    Delivery { target_vcpu_id: usize, source: E },
}

impl<E> IpiRouteError<E> {
    const fn completion(&self) -> RiscvIpiCompletion {
        match self {
            Self::InvalidTarget(_) => RiscvIpiCompletion::InvalidParameter,
            Self::Delivery { .. } => RiscvIpiCompletion::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IpiTargetError {
    HartIdOverflow,
    UnavailableHart(usize),
    DuplicateVcpu(usize),
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
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

    #[test]
    fn selected_harts_publish_level_vssip_in_mask_order() {
        let mut published = Vec::new();

        route_hart_mask(
            0b101,
            4,
            Vec::new,
            |hart_id| match hart_id {
                4 => Some(2),
                6 => Some(0),
                _ => None,
            },
            |target_vcpu_id, interrupt| {
                published.push((target_vcpu_id, interrupt));
                Ok::<_, ()>(())
            },
        )
        .unwrap();

        let expected_interrupt = PendingVcpuInterrupt {
            id: SUPERVISOR_SOFTWARE_INTERRUPT_ID,
            trigger: InterruptTriggerMode::LevelTriggered,
        };
        assert_eq!(
            published,
            [(2, expected_interrupt), (0, expected_interrupt)]
        );
    }

    #[test]
    fn broadcast_publishes_to_every_available_vcpu() {
        let mut published = Vec::new();

        route_hart_mask(
            0,
            usize::MAX,
            || std::vec![2, 0, 1],
            |_| panic!("broadcast must not resolve individual hart IDs"),
            |target_vcpu_id, _interrupt| {
                published.push(target_vcpu_id);
                Ok::<_, ()>(())
            },
        )
        .unwrap();

        assert_eq!(published, [2, 0, 1]);
    }

    #[test]
    fn empty_mask_succeeds_without_resolution_or_publication() {
        let mut published = Vec::new();

        route_hart_mask(
            0,
            0,
            || panic!("ordinary empty mask must not enumerate all vCPUs"),
            |_| panic!("empty mask must not resolve a hart ID"),
            |target_vcpu_id, _interrupt| {
                published.push(target_vcpu_id);
                Ok::<_, ()>(())
            },
        )
        .unwrap();

        assert!(published.is_empty());
    }

    #[test]
    fn unavailable_hart_rejects_the_whole_request_before_publication() {
        let mut published = Vec::new();

        let error = route_hart_mask(
            0b11,
            4,
            Vec::new,
            |hart_id| (hart_id == 4).then_some(2),
            |target_vcpu_id, _interrupt| {
                published.push(target_vcpu_id);
                Ok::<_, ()>(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            IpiRouteError::InvalidTarget(IpiTargetError::UnavailableHart(5))
        ));
        assert_eq!(error.completion(), RiscvIpiCompletion::InvalidParameter);
        assert!(published.is_empty());
    }

    #[test]
    fn overflowing_hart_id_rejects_the_whole_request_before_publication() {
        let mut published = Vec::new();

        let error = route_hart_mask(
            1 << 2,
            usize::MAX - 1,
            Vec::new,
            |_| Some(0),
            |target_vcpu_id, _interrupt| {
                published.push(target_vcpu_id);
                Ok::<_, ()>(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            IpiRouteError::InvalidTarget(IpiTargetError::HartIdOverflow)
        ));
        assert_eq!(error.completion(), RiscvIpiCompletion::InvalidParameter);
        assert!(published.is_empty());
    }

    #[test]
    fn duplicate_vcpu_mapping_rejects_the_whole_request_before_publication() {
        let mut published = Vec::new();

        let error = route_hart_mask(
            0b11,
            4,
            Vec::new,
            |_| Some(2),
            |target_vcpu_id, _interrupt| {
                published.push(target_vcpu_id);
                Ok::<_, ()>(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            IpiRouteError::InvalidTarget(IpiTargetError::DuplicateVcpu(2))
        ));
        assert_eq!(error.completion(), RiscvIpiCompletion::InvalidParameter);
        assert!(published.is_empty());
    }

    #[test]
    fn delivery_failure_reports_failed_and_keeps_the_published_prefix() {
        let mut published = Vec::new();

        let error = route_hart_mask(
            0b111,
            0,
            Vec::new,
            |hart_id| Some(hart_id + 4),
            |target_vcpu_id, _interrupt| {
                published.push(target_vcpu_id);
                if target_vcpu_id == 5 {
                    Err("queue closed")
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            IpiRouteError::Delivery {
                target_vcpu_id: 5,
                source: "queue closed",
            }
        ));
        assert_eq!(error.completion(), RiscvIpiCompletion::Failed);
        assert_eq!(published, [4, 5]);
    }
}
