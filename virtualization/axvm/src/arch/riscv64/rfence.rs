//! Guest RFENCE validation and synchronous host-CPU invalidation.

use rustsbi::SbiRet;
use sbi_spec::rfnc;

use super::{hsm::HartTopology, ipi, policy::RiscvSbiCall};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FenceOperation {
    Instruction,
    GuestTranslation,
}

/// Completes the requested fence before reporting success to the guest.
///
/// All current G-stage roots use VMID zero. Flush every virtualization-enabled
/// host CPU, including CPUs outside the selected guest hart set: a vCPU may
/// have migrated there, and other VMs share the same hardware translation tag.
/// Over-invalidation preserves the requested range and ASID semantics.
pub(super) fn handle(
    call: RiscvSbiCall,
    topology: &HartTopology,
    host_cpus: usize,
    mut execute: impl FnMut(usize, FenceOperation) -> bool,
) -> SbiRet {
    let operation = match call.function {
        rfnc::REMOTE_FENCE_I => FenceOperation::Instruction,
        rfnc::REMOTE_SFENCE_VMA | rfnc::REMOTE_SFENCE_VMA_ASID => FenceOperation::GuestTranslation,
        // VS guests have no nested H-extension or host VMID authority.
        _ => return SbiRet::not_supported(),
    };
    let [mask, base, start, size, ..] = call.arguments;
    let targets = match ipi::resolve_targets(
        mask,
        base,
        || topology.vcpu_ids(),
        |hart| topology.resolve_hart(hart),
    ) {
        Ok(targets) => targets,
        Err(_) => return SbiRet::invalid_param(),
    };
    if operation == FenceOperation::GuestTranslation
        && size != usize::MAX
        && start.checked_add(size).is_none()
    {
        return SbiRet::invalid_address();
    }
    if targets.is_empty() {
        return SbiRet::success(0);
    }
    if host_cpus == 0 {
        return SbiRet::failed();
    }
    let mut remaining = host_cpus;
    while remaining != 0 {
        let cpu = remaining.trailing_zeros() as usize;
        remaining &= remaining - 1;
        if !execute(cpu, operation) {
            return SbiRet::failed();
        }
    }
    SbiRet::success(0)
}

/// Runs bounded register-only work through the existing synchronous IPI path.
pub(super) fn execute(cpu: usize, operation: FenceOperation) -> bool {
    if cpu >= usize::BITS as usize
        || crate::percpu::enabled_cpu_mask() & (1usize << cpu) == 0
        || !ax_cpu::interrupt::irqs_enabled()
    {
        // Synchronous reciprocal calls need the caller's host IPI enabled.
        // The capability check also protects the callback's H instructions.
        return false;
    }
    let mut operation = operation;
    crate::host::task::run_on_cpu_sync(
        cpu,
        execute_local,
        core::ptr::from_mut(&mut operation).cast(),
    )
    .is_ok()
}

/// # Safety
/// `argument` points to a live `FenceOperation` until synchronous completion.
/// The selected CPU must have enabled H virtualization. The callback must run
/// without migration and must not overlap guest instruction execution.
unsafe fn execute_local(argument: *mut ()) {
    // The local fast path also needs to exclude IRQs and migration; remote
    // callbacks already run in the host IPI handler after guest trap entry.
    let _guard = ax_std::os::arceos::guard::PreemptIrqSaveGuard::new();
    // SAFETY: execute() retains its stack argument until run_on_cpu_sync returns;
    // no caller mutates it during the synchronous callback.
    match unsafe { *argument.cast::<FenceOperation>() } {
        FenceOperation::Instruction => riscv::asm::fence_i(),
        FenceOperation::GuestTranslation => {
            // SAFETY: only CPUs in enabled_cpu_mask are selected. The guard
            // excludes migration and guest/IRQ reentry; all guest roots use 0.
            unsafe { ax_cpu::virtualization::invalidate_vsstage_translations(0) };
        }
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use super::*;

    fn call(function: usize, mask: usize, base: usize) -> RiscvSbiCall {
        RiscvSbiCall {
            extension: rfnc::EID_RFNC,
            function,
            arguments: [mask, base, 0x1000, 0x1000, 3, 0],
        }
    }

    #[test]
    fn guest_fences_validate_topology_and_complete_on_all_possible_host_owners() {
        let topology = HartTopology::new(&[(0, Some(1 << 3), 8), (1, Some(1 << 5), 9)]);
        let mut completed = std::vec::Vec::new();
        for (function, operation) in [
            (rfnc::REMOTE_FENCE_I, FenceOperation::Instruction),
            (rfnc::REMOTE_SFENCE_VMA, FenceOperation::GuestTranslation),
            (
                rfnc::REMOTE_SFENCE_VMA_ASID,
                FenceOperation::GuestTranslation,
            ),
        ] {
            completed.clear();
            let result = handle(call(function, 1, 8), &topology, 0b101000, |cpu, actual| {
                completed.push((cpu, actual));
                true
            });
            assert_eq!(result, SbiRet::success(0));
            assert_eq!(completed, [(3, operation), (5, operation)]);
        }

        for request in [
            call(rfnc::REMOTE_FENCE_I, 0b101, 8),
            call(rfnc::REMOTE_FENCE_I, 2, usize::MAX - 1),
        ] {
            assert_eq!(
                handle(request, &topology, 0b101000, |_, _| panic!(
                    "invalid guest hart must not reach a host CPU"
                )),
                SbiRet::invalid_param()
            );
        }
        for function in rfnc::REMOTE_HFENCE_GVMA_VMID..=rfnc::REMOTE_HFENCE_VVMA {
            assert_eq!(
                handle(call(function, 1, 8), &topology, 0b101000, |_, _| panic!(
                    "nested HFENCE must not reach the host"
                )),
                SbiRet::not_supported()
            );
        }
        assert_eq!(
            handle(
                call(rfnc::REMOTE_FENCE_I, 0, 8),
                &topology,
                0b101000,
                |_, _| panic!("empty target set")
            ),
            SbiRet::success(0)
        );
        completed.clear();
        assert_eq!(
            handle(
                call(rfnc::REMOTE_SFENCE_VMA, 0, usize::MAX),
                &topology,
                0b101000,
                |cpu, operation| {
                    completed.push((cpu, operation));
                    cpu != 5
                }
            ),
            SbiRet::failed()
        );
        assert_eq!(
            completed,
            [
                (3, FenceOperation::GuestTranslation),
                (5, FenceOperation::GuestTranslation)
            ]
        );
    }
}

#[cfg(all(feature = "fault-injection", not(feature = "host-test")))]
mod hardware_tests;
