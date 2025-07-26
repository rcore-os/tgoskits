use x86::bits64::vmx;
use x86_64::registers::control::{Cr0, Cr4, Cr4Flags};

use axerrno::{AxResult, ax_err, ax_err_type};
use axvcpu::{AxArchPerCpu, AxVCpuHal};
use memory_addr::PAGE_SIZE_4K as PAGE_SIZE;

use crate::msr::Msr;
use crate::vmx::has_hardware_support;
use crate::vmx::structs::{FeatureControl, FeatureControlFlags, VmxBasic, VmxRegion};

/// Represents the per-CPU state for Virtual Machine Extensions (VMX).
///
/// This structure holds the state information specific to a CPU core
/// when operating in VMX mode, including the VMCS revision identifier and
/// the VMX region.
#[derive(Debug)]
pub struct VmxPerCpuState<H: AxVCpuHal> {
    /// The VMCS (Virtual Machine Control Structure) revision identifier.
    ///
    /// This identifier is used to ensure compatibility between the software
    /// and the specific version of the VMCS that the CPU supports.
    pub(crate) vmcs_revision_id: u32,

    /// The VMX region for this CPU.
    ///
    /// This region typically contains the VMCS and other state information
    /// required for managing virtual machines on this particular CPU.
    vmx_region: VmxRegion<H::MmHal>,
}

impl<H: AxVCpuHal> AxArchPerCpu for VmxPerCpuState<H> {
    fn new(_cpu_id: usize) -> AxResult<Self> {
        Ok(Self {
            vmcs_revision_id: 0,
            vmx_region: unsafe { VmxRegion::uninit() },
        })
    }

    fn is_enabled(&self) -> bool {
        Cr4::read().contains(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS)
    }

    fn hardware_enable(&mut self) -> AxResult {
        if !has_hardware_support() {
            return ax_err!(Unsupported, "CPU does not support feature VMX");
        }
        if self.is_enabled() {
            return ax_err!(ResourceBusy, "VMX is already turned on");
        }

        // Enable XSAVE/XRSTOR.
        super::vcpu::XState::enable_xsave();

        // Enable VMXON, if required.
        let ctrl = FeatureControl::read();
        let locked = ctrl.contains(FeatureControlFlags::LOCKED);
        let vmxon_outside = ctrl.contains(FeatureControlFlags::VMXON_ENABLED_OUTSIDE_SMX);
        if !locked {
            FeatureControl::write(
                ctrl | FeatureControlFlags::LOCKED | FeatureControlFlags::VMXON_ENABLED_OUTSIDE_SMX,
            )
        } else if !vmxon_outside {
            return ax_err!(Unsupported, "VMX disabled by BIOS");
        }

        // Check control registers are in a VMX-friendly state. (SDM Vol. 3C, Appendix A.7, A.8)
        macro_rules! cr_is_valid {
            ($value: expr, $crx: ident) => {{
                use Msr::*;
                let value = $value;
                paste::paste! {
                    let fixed0 = [<IA32_VMX_ $crx _FIXED0>].read();
                    let fixed1 = [<IA32_VMX_ $crx _FIXED1>].read();
                }
                (!fixed0 | value != 0) && (fixed1 | !value != 0)
            }};
        }
        if !cr_is_valid!(Cr0::read().bits(), CR0) {
            return ax_err!(BadState, "host CR0 is not valid in VMX operation");
        }
        if !cr_is_valid!(Cr4::read().bits(), CR4) {
            return ax_err!(BadState, "host CR4 is not valid in VMX operation");
        }

        // Get VMCS revision identifier in IA32_VMX_BASIC MSR.
        let vmx_basic = VmxBasic::read();
        if vmx_basic.region_size as usize != PAGE_SIZE {
            return ax_err!(Unsupported);
        }
        if vmx_basic.mem_type != VmxBasic::VMX_MEMORY_TYPE_WRITE_BACK {
            return ax_err!(Unsupported);
        }
        if vmx_basic.is_32bit_address {
            return ax_err!(Unsupported);
        }
        if !vmx_basic.io_exit_info {
            return ax_err!(Unsupported);
        }
        if !vmx_basic.vmx_flex_controls {
            return ax_err!(Unsupported);
        }
        self.vmcs_revision_id = vmx_basic.revision_id;
        self.vmx_region = VmxRegion::new(self.vmcs_revision_id, false)?;

        unsafe {
            // Enable VMX using the VMXE bit.
            Cr4::write(Cr4::read() | Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS);
            // Execute VMXON.
            vmx::vmxon(self.vmx_region.phys_addr().as_usize() as _).map_err(|err| {
                ax_err_type!(
                    BadState,
                    format_args!("VMX instruction vmxon failed: {:?}", err)
                )
            })?;
        }
        info!("[AxVM] succeeded to turn on VMX.");

        Ok(())
    }

    fn hardware_disable(&mut self) -> AxResult {
        if !self.is_enabled() {
            return ax_err!(BadState, "VMX is not enabled");
        }

        unsafe {
            // Execute VMXOFF.
            vmx::vmxoff().map_err(|err| {
                ax_err_type!(
                    BadState,
                    format_args!("VMX instruction vmxoff failed: {:?}", err)
                )
            })?;
            // Remove VMXE bit in CR4.
            Cr4::update(|cr4| cr4.remove(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS));
        };
        info!("[AxVM] succeeded to turn off VMX.");

        self.vmx_region = unsafe { VmxRegion::uninit() };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::mock::{MockMmHal, MockVCpuHal};
    use alloc::format;
    use alloc::vec::Vec;

    #[test]
    fn test_vmx_per_cpu_state_new() {
        MockMmHal::reset(); // Reset before test
        let result = VmxPerCpuState::<MockVCpuHal>::new(0);
        assert!(result.is_ok());

        let state = result.unwrap();
        assert_eq!(state.vmcs_revision_id, 0);
    }

    #[test]
    fn test_vmx_per_cpu_state_default_values() {
        MockMmHal::reset(); // Reset before test
        let state = VmxPerCpuState::<MockVCpuHal>::new(0).unwrap();

        // Test that vmcs_revision_id is initialized to 0
        assert_eq!(state.vmcs_revision_id, 0);

        // The VMX region should be in an uninitialized state
        // We can't test this directly as the field is private,
        // but we can ensure the struct is created successfully
    }

    #[test]
    fn test_multiple_cpu_states_independence() {
        MockMmHal::reset(); // Reset before test
        let mut states = Vec::new();

        // Create states for multiple CPUs
        for cpu_id in 0..4 {
            let state = VmxPerCpuState::<MockVCpuHal>::new(cpu_id).unwrap();
            states.push(state);
        }

        // Test independence by modifying one state and verifying others are unaffected
        states[0].vmcs_revision_id = 0x12345678;
        states[1].vmcs_revision_id = 0x87654321;

        // Verify each state maintains its own value
        assert_eq!(states[0].vmcs_revision_id, 0x12345678);
        assert_eq!(states[1].vmcs_revision_id, 0x87654321);
        assert_eq!(states[2].vmcs_revision_id, 0);
        assert_eq!(states[3].vmcs_revision_id, 0);
    }

    #[test]
    fn test_vmx_per_cpu_state_debug() {
        MockMmHal::reset(); // Reset before test
        let state = VmxPerCpuState::<MockVCpuHal>::new(0).unwrap();

        // Test that Debug trait is implemented and doesn't panic
        let debug_str = format!("{:?}", state);
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_vmx_per_cpu_state_size() {
        use core::mem;

        // Test that the struct has a reasonable size
        let size = mem::size_of::<VmxPerCpuState<MockVCpuHal>>();

        // Should be larger than just the u32 field due to the VmxRegion
        assert!(size > 4);

        // But shouldn't be excessively large (this is a sanity check)
        assert!(size < 1024);
    }
}
