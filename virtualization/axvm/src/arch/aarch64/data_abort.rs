//! AArch64 data-abort ownership and emulation policy.

use arm_vcpu::{ArmDataAbort, ArmDataAccess, ArmDataAccessResult, ArmDataFault};
use axdevice::DeviceManagerError;
use axdevice_base::DeviceError;
use axvm_types::{GuestPhysAddr, MappingFlags};

use super::{
    Aarch64Arch, Aarch64DeferredRunWork, AxvmArmVcpu, arm_access_width_to_ax,
    arm_guest_phys_addr_to_ax, nested_page_fault,
};
use crate::{
    AxVmError, AxVmResult,
    architecture::{ArchOps, BoundVcpuExit},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DataAbortAddressOwner {
    Device,
    Stage2(MappingFlags),
    Unassigned,
}

pub(super) fn handle(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmArmVcpu>,
    abort: ArmDataAbort,
) -> AxVmResult<BoundVcpuExit<Aarch64DeferredRunWork>> {
    let Some(fault_ipa) = abort.fault_ipa() else {
        return inject_guest_abort(vm, vcpu, abort, "fault IPA is unavailable");
    };
    let exact_address = fault_ipa.exact_address();
    let address = arm_guest_phys_addr_to_ax(exact_address.unwrap_or_else(|| fault_ipa.page_base()));
    let devices = vm.get_devices()?;
    let mapping_flags =
        vm.with_resources(|resources| Ok(resources.address_space.mapping_flags_at(address)))?;
    let owner = classify_address_owner(devices.owns_mmio_address(address), mapping_flags)?;

    match owner {
        DataAbortAddressOwner::Device => {
            let Some(address) = exact_address.map(arm_guest_phys_addr_to_ax) else {
                return inject_guest_abort(
                    vm,
                    vcpu,
                    abort,
                    "MMIO fault has no architecturally valid IPA byte offset",
                );
            };
            handle_device_access(vm, vcpu, &devices, abort, address)
        }
        DataAbortAddressOwner::Stage2(mapping_flags) => {
            handle_stage2_fault(vm, vcpu, abort, address, mapping_flags)
        }
        DataAbortAddressOwner::Unassigned => {
            inject_guest_abort(vm, vcpu, abort, "guest physical address is unassigned")
        }
    }
}

fn handle_device_access(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmArmVcpu>,
    devices: &axdevice::AxVmDevices,
    abort: ArmDataAbort,
    address: GuestPhysAddr,
) -> AxVmResult<BoundVcpuExit<Aarch64DeferredRunWork>> {
    if !matches!(abort.syndrome().fault(), ArmDataFault::Translation { .. }) {
        return inject_guest_abort(
            vm,
            vcpu,
            abort,
            "registered MMIO address did not raise a translation fault",
        );
    }
    let Some(access) = abort.access() else {
        return inject_guest_abort(
            vm,
            vcpu,
            abort,
            "data abort has no valid single-register instruction syndrome",
        );
    };
    let completed_write = matches!(access, ArmDataAccess::Write { .. });

    let result = match access {
        ArmDataAccess::Read { width, .. } => devices
            .handle_mmio_read(address, arm_access_width_to_ax(width))
            .map(|value| ArmDataAccessResult::Read(value as u64)),
        ArmDataAccess::Write { width, value, .. } => vm
            .dispatch_mmio_write(
                devices,
                address,
                arm_access_width_to_ax(width),
                value as usize,
            )
            .map(|()| ArmDataAccessResult::Write),
    };

    match result {
        Ok(result) => {
            vcpu.get_arch_vcpu()
                .complete_data_abort(abort, result)
                .map_err(|error| AxVmError::vcpu("complete emulated AArch64 data abort", error))?;
            if completed_write {
                <Aarch64Arch as ArchOps>::after_mmio_write(vm);
            }
            Ok(BoundVcpuExit::Continue)
        }
        Err(error) if is_guest_access_rejection(&error) => {
            warn!(
                "VM[{}] VCpu[{}] rejected guest MMIO access at {:#x}: {error}",
                vm.id(),
                vcpu.id(),
                address.as_usize(),
            );
            inject_guest_abort(vm, vcpu, abort, "device rejected the guest transaction")
        }
        Err(error) => Err(AxVmError::device("emulate AArch64 guest MMIO", error)),
    }
}

fn handle_stage2_fault(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmArmVcpu>,
    abort: ArmDataAbort,
    address: GuestPhysAddr,
    mapping_flags: MappingFlags,
) -> AxVmResult<BoundVcpuExit<Aarch64DeferredRunWork>> {
    if !matches!(abort.syndrome().fault(), ArmDataFault::Translation { .. }) {
        return inject_guest_abort(
            vm,
            vcpu,
            abort,
            "mapped address raised a non-translation data abort",
        );
    }
    let required_flags = required_mapping_flags(&abort);
    if !mapping_flags.contains(required_flags) {
        return inject_guest_abort(
            vm,
            vcpu,
            abort,
            "stage-2 mapping does not authorize the guest access",
        );
    }

    match nested_page_fault::resolve(vm, address, required_flags)? {
        nested_page_fault::NestedPageFaultResolution::Resolved => Ok(BoundVcpuExit::Continue),
        nested_page_fault::NestedPageFaultResolution::OwnedButUnresolved { mapping_flags } => {
            Err(AxVmError::memory(
                "resolve owned AArch64 stage-2 fault",
                alloc::format!(
                    "GPA {:#x} is owned with flags {mapping_flags:?}, but its mapping could not \
                     be restored",
                    address.as_usize(),
                ),
            ))
        }
        nested_page_fault::NestedPageFaultResolution::Unassigned => Err(AxVmError::invalid_state(
            "resolve AArch64 stage-2 fault",
            alloc::format!(
                "GPA {:#x} lost address-space ownership during fault handling",
                address.as_usize(),
            ),
        )),
    }
}

fn inject_guest_abort(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmArmVcpu>,
    abort: ArmDataAbort,
    reason: &'static str,
) -> AxVmResult<BoundVcpuExit<Aarch64DeferredRunWork>> {
    warn!(
        "VM[{}] VCpu[{}] injects external data abort: {reason}; IPA={:?}, FAR={:?}, ESR={:#x}, \
         PC={:#x}",
        vm.id(),
        vcpu.id(),
        abort.fault_ipa(),
        abort.fault_virtual_address(),
        abort.syndrome().raw_esr(),
        abort.instruction_address(),
    );
    vcpu.get_arch_vcpu()
        .inject_external_data_abort(abort)
        .map_err(|error| AxVmError::vcpu("inject AArch64 external data abort", error))?;
    Ok(BoundVcpuExit::Continue)
}

fn classify_address_owner(
    device_owned: bool,
    mapping_flags: Option<MappingFlags>,
) -> AxVmResult<DataAbortAddressOwner> {
    match (device_owned, mapping_flags) {
        (true, Some(mapping_flags)) => Err(AxVmError::invalid_state(
            "classify AArch64 data-abort address",
            alloc::format!(
                "MMIO trap window overlaps a stage-2 mapping with flags {mapping_flags:?}"
            ),
        )),
        (true, None) => Ok(DataAbortAddressOwner::Device),
        (false, Some(mapping_flags)) => Ok(DataAbortAddressOwner::Stage2(mapping_flags)),
        (false, None) => Ok(DataAbortAddressOwner::Unassigned),
    }
}

fn required_mapping_flags(abort: &ArmDataAbort) -> MappingFlags {
    if abort.syndrome().is_write() {
        MappingFlags::WRITE
    } else {
        MappingFlags::READ
    }
}

fn is_guest_access_rejection(error: &DeviceManagerError) -> bool {
    matches!(
        error,
        DeviceManagerError::Access {
            source: DeviceError::NotFound
                | DeviceError::InvalidWidth { .. }
                | DeviceError::ReadOnly
                | DeviceError::WriteOnly
                | DeviceError::OutOfRange { .. },
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_registered_trap_window_as_device() {
        assert_eq!(
            classify_address_owner(true, None).unwrap(),
            DataAbortAddressOwner::Device
        );
        assert_eq!(
            classify_address_owner(false, None).unwrap(),
            DataAbortAddressOwner::Unassigned
        );
    }

    #[test]
    fn rejects_overlapping_device_and_stage2_ownership() {
        assert!(
            classify_address_owner(true, Some(MappingFlags::READ)).is_err(),
            "one GPA must not be both directly mapped and trapped"
        );
    }
}
