// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub use ax_cpu::virtualization::{
    ApicAccessExitInfo, ApicAccessExitType, CrAccessInfo, VmcsControl32, VmcsControl64,
    VmcsControlNW, VmcsGuest16, VmcsGuest32, VmcsGuest64, VmcsGuestNW, VmcsReadOnly32,
    VmcsReadOnly64, VmcsReadOnlyNW, VmxExitInfo, VmxInterruptInfo, VmxIoExitInfo,
};
use ax_cpu::virtualization::{ControlMemory, VmxControls};
use bit_field::BitField;

use super::VmxInterruptionType;
use crate::arch::x86_64::policy::{
    X86AccessFlags, X86GuestPhysAddr, X86HostPhysAddr, X86NestedPageFaultInfo, X86VcpuError,
    X86VcpuResult,
    pending_event::{PendingEvent, PendingEventKind},
};

pub mod controls {
    pub use x86::vmx::vmcs::control::{
        EntryControls, ExitControls, PinbasedControls, PrimaryControls, SecondaryControls,
    };
}

pub fn set_ept_pointer<M: ControlMemory>(
    controls: &mut VmxControls<M>,
    pml4_paddr: X86HostPhysAddr,
) -> X86VcpuResult {
    // SAFETY: AxVM owns a bound VMX CPU and retains the aligned EPT root.
    // No VM policy consumes hardware A/D tracking, so do not require it.
    let pointer = unsafe {
        ax_cpu::virtualization::EptPointer::for_current_cpu(pml4_paddr.as_usize().into(), false)?
    };
    controls.write(VmcsControl64::EPTP, pointer.bits())?;
    // SAFETY: the enabled CPU and root remain owned through invalidation.
    unsafe { pointer.invalidate()? };

    Ok(())
}

pub fn inject_event<M: ControlMemory>(
    controls: &mut VmxControls<M>,
    event: PendingEvent,
) -> X86VcpuResult {
    // SDM Vol. 3C, Section 24.8.3
    let err_code =
        if event.is_exception() && VmxInterruptionType::vector_has_error_code(event.vector) {
            Some(match event.err_code {
                Some(err_code) => err_code,
                None => controls
                    .read(VmcsReadOnly32::VMEXIT_INTERRUPTION_ERR_CODE)
                    .map_err(X86VcpuError::from)?,
            })
        } else {
            None
        };
    let length = controls.read(VmcsReadOnly32::VMEXIT_INSTRUCTION_LEN)?;
    controls
        .inject_interrupt(event_interrupt_info(event, err_code), length)
        .map_err(X86VcpuError::from)
}

pub(super) const fn interruption_type_for_event(event: PendingEvent) -> VmxInterruptionType {
    match event.kind {
        PendingEventKind::ExternalInterrupt(_) => VmxInterruptionType::External,
        PendingEventKind::Exception => VmxInterruptionType::HardException,
    }
}

fn event_interrupt_info(event: PendingEvent, err_code: Option<u32>) -> VmxInterruptInfo {
    VmxInterruptInfo {
        vector: event.vector,
        int_type: interruption_type_for_event(event),
        err_code: event.is_exception().then_some(err_code).flatten(),
        valid: true,
    }
}

pub fn ept_violation_info<M: ControlMemory>(
    controls: &VmxControls<M>,
) -> X86VcpuResult<X86NestedPageFaultInfo> {
    // SDM Vol. 3C, Section 27.2.1, Table 27-7
    let qualification = controls
        .read(VmcsReadOnlyNW::EXIT_QUALIFICATION)
        .map_err(X86VcpuError::from)?;
    let fault_guest_paddr = controls
        .read(VmcsReadOnly64::GUEST_PHYSICAL_ADDR)
        .map_err(X86VcpuError::from)? as usize;
    let mut access_flags = X86AccessFlags::empty();
    if qualification.get_bit(0) {
        access_flags |= X86AccessFlags::READ;
    }
    if qualification.get_bit(1) {
        access_flags |= X86AccessFlags::WRITE;
    }
    if qualification.get_bit(2) {
        access_flags |= X86AccessFlags::EXECUTE;
    }
    Ok(X86NestedPageFaultInfo {
        access_flags,
        fault_guest_paddr: X86GuestPhysAddr::from(fault_guest_paddr),
    })
}

pub fn cr_access_info<M: ControlMemory>(controls: &VmxControls<M>) -> X86VcpuResult<CrAccessInfo> {
    let qualification = controls
        .read(VmcsReadOnlyNW::EXIT_QUALIFICATION)
        .map_err(X86VcpuError::from)?;
    Ok(CrAccessInfo::decode(qualification))
}

pub fn apic_access_exit_info<M: ControlMemory>(
    controls: &VmxControls<M>,
) -> X86VcpuResult<ApicAccessExitInfo> {
    let qualification = controls
        .read(VmcsReadOnlyNW::EXIT_QUALIFICATION)
        .map_err(X86VcpuError::from)?;
    // debug!("apic_access_info qualification {:#x}", qualification);

    Ok(ApicAccessExitInfo {
        offset: qualification.get_bits(0..12) as u16,
        access_type: ApicAccessExitType::try_from(qualification.get_bits(12..16) as u8).unwrap(),
        non_event_delivery_asynchronous: qualification.get_bit(16),
    })
}

#[cfg(test)]
mod tests {
    use super::{VmxInterruptionType, event_interrupt_info};
    use crate::arch::x86_64::policy::pending_event::{PendingEvent, PendingEventSource};

    #[test]
    fn vmx_injection_type_and_error_code_follow_event_kind_not_vector() {
        let pic = PendingEvent::external_interrupt(8, false, PendingEventSource::LegacyPic);
        let pic_info = event_interrupt_info(pic, None);
        assert_eq!(pic_info.int_type, VmxInterruptionType::External);
        assert_eq!(pic_info.err_code, None);
        assert_eq!(pic_info.bits() & (1 << 11), 0);

        let double_fault = PendingEvent::exception(8, Some(0));
        let exception_info = event_interrupt_info(double_fault, Some(0));
        assert_eq!(exception_info.int_type, VmxInterruptionType::HardException);
        assert_eq!(exception_info.err_code, Some(0));
        assert_ne!(exception_info.bits() & (1 << 11), 0);
    }
}
