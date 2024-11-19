use x86::bits64::vmx;
use x86_64::registers::control::{Cr0, Cr4, Cr4Flags};

use axerrno::{ax_err, ax_err_type, AxResult};
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
    vmx_region: VmxRegion<H>,
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
                let fixed0 = concat_idents!(IA32_VMX_, $crx, _FIXED0).read();
                let fixed1 = concat_idents!(IA32_VMX_, $crx, _FIXED1).read();
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
