//! Architecture-neutral handlers for exits shared by every guest architecture.

use axdevice_base::{BusKind, DeviceAccess, DeviceVcpuId};
use axvm_types::VmArchVcpuOps;

use super::{
    BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuEventWait, VcpuRunAction,
};
use crate::{AxVmError, AxVmResult, StopReason};

pub(crate) fn handle_mmio_read<V: VmArchVcpuOps, D>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: MmioReadExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    if !try_handle_mmio_read(vm, vcpu, exit)? {
        return Err(missing_mmio_error("read", exit.addr, exit.width));
    }
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn try_handle_mmio_read<V: VmArchVcpuOps>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: MmioReadExit,
) -> AxVmResult<bool> {
    let Some(raw) = try_read_mmio_value(vm, vcpu, exit.addr, exit.width)? else {
        return Ok(false);
    };
    let masked = raw as usize & crate::vm::width_mask(exit.width);
    let val = if exit.signed_ext {
        crate::vm::sign_extend_value(masked, exit.width)
    } else {
        masked & crate::vm::width_mask(exit.reg_width)
    };
    vcpu.set_gpr(exit.reg, val);
    Ok(true)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn read_mmio_value<V: VmArchVcpuOps>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    addr: axvm_types::GuestPhysAddr,
    width: axvm_types::AccessWidth,
) -> AxVmResult<usize> {
    match try_read_mmio_value(vm, vcpu, addr, width)? {
        Some(raw) => Ok(raw as usize),
        None => Err(missing_mmio_error("read", addr, width)),
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
pub(crate) fn finish_external_interrupt(vector: usize) {
    crate::host::arceos::dispatch_host_irq(vector);
}

pub(crate) fn try_read_mmio_value<V: VmArchVcpuOps>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<V>,
    addr: axvm_types::GuestPhysAddr,
    width: axvm_types::AccessWidth,
) -> AxVmResult<Option<u64>> {
    let access = DeviceAccess::new(
        DeviceVcpuId::new(vcpu.id()),
        BusKind::Mmio,
        addr.as_usize() as u64,
        width,
    );
    vm.get_devices()?
        .try_read(&access)
        .map_err(|error| AxVmError::device("read guest MMIO", error))
}

pub(crate) fn handle_mmio_write<V: VmArchVcpuOps, D>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: MmioWriteExit,
) -> AxVmResult<BoundVcpuExit<D>> {
    if !try_handle_mmio_write(vm, vcpu, exit)? {
        return Err(missing_mmio_error("write", exit.addr, exit.width));
    }
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn try_handle_mmio_write<V: VmArchVcpuOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: MmioWriteExit,
) -> AxVmResult<bool> {
    let access = DeviceAccess::new(
        DeviceVcpuId::new(vcpu.id()),
        BusKind::Mmio,
        exit.addr.as_usize() as u64,
        exit.width,
    );
    vm.try_write_device(&access, exit.data)
}

fn missing_mmio_error(
    operation: &'static str,
    addr: axvm_types::GuestPhysAddr,
    width: axvm_types::AccessWidth,
) -> AxVmError {
    AxVmError::device(
        "access guest MMIO",
        axdevice::DeviceManagerError::Access {
            operation,
            bus: axdevice_base::BusKind::Mmio,
            addr: addr.as_usize() as u64,
            width,
            source: axdevice_base::DeviceError::NotFound,
        },
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HyperCallExitAction {
    Return(usize),
    Complete(VcpuRunAction),
    CompleteWithReturn {
        return_value: usize,
        action: VcpuRunAction,
    },
}

const PSCI_32_BASE: u64 = 0x8400_0000;
const PSCI_32_END: u64 = 0x8400_0020;
const PSCI_64_BASE: u64 = 0xc400_0000;
const PSCI_64_END: u64 = 0xc400_0020;
const PSCI_RET_NOT_SUPPORTED: usize = usize::MAX;

fn is_aarch64_psci_function_id(raw_code: u64, abi: crate::runtime::hvc::HyperCallAbi) -> bool {
    abi == crate::runtime::hvc::HyperCallAbi::AArch64
        && ((PSCI_32_BASE..PSCI_32_END).contains(&raw_code)
            || (PSCI_64_BASE..PSCI_64_END).contains(&raw_code))
}

fn complete_hypercall_decode_error<V: VmArchVcpuOps>(
    vcpu: &crate::vcpu::AxVCpu<V>,
    raw_code: u64,
    abi: crate::runtime::hvc::HyperCallAbi,
) {
    if is_aarch64_psci_function_id(raw_code, abi) {
        vcpu.set_return_value(PSCI_RET_NOT_SUPPORTED);
    }
}

pub(crate) fn hvc_outcome_action(
    outcome: crate::runtime::hvc::HyperCallOutcome,
) -> HyperCallExitAction {
    match outcome {
        crate::runtime::hvc::HyperCallOutcome::Return(ret_val) => {
            HyperCallExitAction::Return(ret_val)
        }
        crate::runtime::hvc::HyperCallOutcome::CpuSuspendStandby { return_value } => {
            HyperCallExitAction::CompleteWithReturn {
                return_value,
                action: VcpuRunAction {
                    event_wait: VcpuEventWait::Poll,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                },
            }
        }
        crate::runtime::hvc::HyperCallOutcome::CpuOff => {
            HyperCallExitAction::Complete(VcpuRunAction {
                event_wait: VcpuEventWait::None,
                stop_reason: None,
                resets_vm: false,
                exits_vcpu: true,
            })
        }
        crate::runtime::hvc::HyperCallOutcome::SystemReset => {
            HyperCallExitAction::Complete(VcpuRunAction {
                event_wait: VcpuEventWait::None,
                stop_reason: None,
                resets_vm: true,
                exits_vcpu: false,
            })
        }
        crate::runtime::hvc::HyperCallOutcome::SystemOff => {
            HyperCallExitAction::Complete(VcpuRunAction {
                event_wait: VcpuEventWait::None,
                stop_reason: Some(StopReason::SystemDown),
                resets_vm: false,
                exits_vcpu: false,
            })
        }
    }
}

pub(crate) fn handle_hypercall<V: VmArchVcpuOps, D>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: HypercallExit,
    abi: crate::runtime::hvc::HyperCallAbi,
) -> AxVmResult<BoundVcpuExit<D>> {
    debug!("Hypercall [{:#x}] args {:x?}", exit.nr, exit.args);
    match crate::runtime::hvc::HyperCall::new(vm.clone(), exit.nr, exit.args, abi) {
        Ok(hypercall) => match hypercall.execute() {
            Ok(outcome) => match hvc_outcome_action(outcome) {
                HyperCallExitAction::Return(ret_val) => {
                    vcpu.set_return_value(ret_val);
                }
                HyperCallExitAction::CompleteWithReturn {
                    return_value,
                    action,
                } => {
                    vcpu.set_return_value(return_value);
                    return Ok(BoundVcpuExit::Complete(action));
                }
                HyperCallExitAction::Complete(action) => {
                    return Ok(BoundVcpuExit::Complete(action));
                }
            },
            Err(error) => {
                let err = AxVmError::from(error);
                warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
                vcpu.set_return_value(usize::MAX);
            }
        },
        Err(error) => {
            let err = AxVmError::from(error);
            warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
            complete_hypercall_decode_error(vcpu, exit.nr, abi);
        }
    }
    Ok(BoundVcpuExit::Complete(VcpuRunAction {
        event_wait: VcpuEventWait::None,
        stop_reason: None,
        resets_vm: false,
        exits_vcpu: false,
    }))
}

#[cfg(test)]
mod tests {

    #[derive(Debug)]
    struct UnknownPsciExit;

    struct UnknownPsciVcpu {
        return_value: usize,
    }

    impl VmArchVcpuOps for UnknownPsciVcpu {
        type CreateConfig = ();
        type SetupConfig = ();
        type Exit = UnknownPsciExit;

        fn new(
            _vm_id: axvm_types::VMId,
            _vcpu_id: axvm_types::VCpuId,
            _config: Self::CreateConfig,
        ) -> Result<Self, axvm_types::VmBackendError> {
            Ok(Self { return_value: 0 })
        }

        fn set_entry(
            &mut self,
            _entry: axvm_types::GuestPhysAddr,
        ) -> Result<(), axvm_types::VmBackendError> {
            Ok(())
        }

        fn set_nested_page_table(
            &mut self,
            _config: axvm_types::NestedPagingConfig,
        ) -> Result<(), axvm_types::VmBackendError> {
            Ok(())
        }

        fn setup(&mut self, _config: Self::SetupConfig) -> Result<(), axvm_types::VmBackendError> {
            Ok(())
        }

        fn run(&mut self) -> Result<Self::Exit, axvm_types::VmBackendError> {
            Ok(UnknownPsciExit)
        }

        fn bind(&mut self) -> Result<(), axvm_types::VmBackendError> {
            Ok(())
        }

        fn unbind(&mut self) -> Result<(), axvm_types::VmBackendError> {
            Ok(())
        }

        fn set_gpr(&mut self, _reg: usize, _val: usize) {}

        fn inject_interrupt(&mut self, _vector: usize) -> Result<(), axvm_types::VmBackendError> {
            Ok(())
        }

        fn set_return_value(&mut self, val: usize) {
            self.return_value = val;
        }
    }

    #[test]
    fn hvc_unknown_aarch64_psci_id_returns_not_supported() {
        let vcpu = crate::vcpu::AxVCpu::<UnknownPsciVcpu>::new(99, 0, None, ()).unwrap();

        complete_hypercall_decode_error(
            &vcpu,
            0x8400_000c,
            crate::runtime::hvc::HyperCallAbi::AArch64,
        );

        assert_eq!(vcpu.get_arch_vcpu().return_value, PSCI_RET_NOT_SUPPORTED);
    }

    #[test]
    fn hvc_unknown_non_aarch64_psci_id_does_not_clobber_return_value() {
        let vcpu = crate::vcpu::AxVCpu::<UnknownPsciVcpu>::new(99, 0, None, ()).unwrap();
        vcpu.set_return_value(0x8400_000c);

        complete_hypercall_decode_error(
            &vcpu,
            0x8400_000c,
            crate::runtime::hvc::HyperCallAbi::Generic,
        );

        assert_eq!(vcpu.get_arch_vcpu().return_value, 0x8400_000c);
    }

    #[test]
    fn hvc_cpu_suspend_standby_sets_success_before_waiting() {
        let action = hvc_outcome_action(HyperCallOutcome::CpuSuspendStandby { return_value: 0 });

        assert_eq!(
            action,
            HyperCallExitAction::CompleteWithReturn {
                return_value: 0,
                action: VcpuRunAction {
                    event_wait: VcpuEventWait::Poll,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                },
            }
        );
    }

    use super::*;
    use crate::runtime::hvc::HyperCallOutcome;

    #[test]
    fn hvc_cpu_suspend_standby_waits_for_event_without_exiting() {
        let action = hvc_outcome_action(HyperCallOutcome::CpuSuspendStandby { return_value: 0 });

        assert_eq!(
            action,
            HyperCallExitAction::CompleteWithReturn {
                return_value: 0,
                action: VcpuRunAction {
                    event_wait: VcpuEventWait::Poll,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                },
            }
        );
    }

    #[test]
    fn hvc_system_reset_requests_vm_reset_without_returning() {
        assert_eq!(
            hvc_outcome_action(HyperCallOutcome::SystemReset),
            HyperCallExitAction::Complete(VcpuRunAction {
                event_wait: VcpuEventWait::None,
                stop_reason: None,
                resets_vm: true,
                exits_vcpu: false,
            })
        );
    }

    #[test]
    fn hvc_cpu_off_exits_current_vcpu_without_stopping_vm() {
        let action = hvc_outcome_action(crate::runtime::hvc::HyperCallOutcome::CpuOff);

        assert_eq!(
            action,
            HyperCallExitAction::Complete(VcpuRunAction {
                event_wait: VcpuEventWait::None,
                stop_reason: None,
                resets_vm: false,
                exits_vcpu: true,
            })
        );
    }
}
