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

use core::marker::PhantomData;

use x86::bits64::vmx;
use x86_64::registers::control::{Cr0, Cr4, Cr4Flags};

use crate::{
    X86HostOps, X86VcpuResult,
    msr::Msr,
    types::X86_PAGE_SIZE_4K as PAGE_SIZE,
    vmx::{
        has_hardware_support,
        structs::{FeatureControl, FeatureControlFlags, VmxBasic, VmxRegion},
    },
    xstate::enable_xsave,
};

/// Represents the per-CPU state for Virtual Machine Extensions (VMX).
///
/// This structure holds the state information specific to a CPU core
/// when operating in VMX mode, including the VMCS revision identifier and
/// the VMX region.
#[derive(Debug)]
pub struct VmxPerCpuState<H: X86HostOps> {
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
    _host: PhantomData<fn() -> H>,
}

impl<H: X86HostOps> VmxPerCpuState<H> {
    pub fn new(_cpu_id: usize) -> X86VcpuResult<Self> {
        Ok(Self {
            vmcs_revision_id: 0,
            vmx_region: unsafe { VmxRegion::<H>::uninit() },
            _host: PhantomData,
        })
    }

    pub fn is_enabled(&self) -> bool {
        Cr4::read().contains(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS)
    }

    pub fn hardware_enable(&mut self) -> X86VcpuResult {
        if !has_hardware_support() {
            return x86_err!(Unsupported, "CPU does not support feature VMX");
        }
        if self.is_enabled() {
            return x86_err!(ResourceBusy, "VMX is already turned on");
        }

        // Enable XSAVE/XRSTOR.
        enable_xsave();

        // Enable VMXON, if required.
        let ctrl = FeatureControl::read();
        let locked = ctrl.contains(FeatureControlFlags::LOCKED);
        let vmxon_outside = ctrl.contains(FeatureControlFlags::VMXON_ENABLED_OUTSIDE_SMX);
        if !locked {
            FeatureControl::write(
                ctrl | FeatureControlFlags::LOCKED | FeatureControlFlags::VMXON_ENABLED_OUTSIDE_SMX,
            )
        } else if !vmxon_outside {
            return x86_err!(Unsupported, "VMX disabled by BIOS");
        }

        let cr0 = Cr0::read_raw();
        let cr0_fixed0 = Msr::IA32_VMX_CR0_FIXED0.read();
        let cr0_fixed1 = Msr::IA32_VMX_CR0_FIXED1.read();
        let cr0_vmx = vmx_fixed_control_value(cr0, cr0_fixed0, cr0_fixed1);
        let cr4 = Cr4::read_raw();
        let cr4_fixed0 = Msr::IA32_VMX_CR4_FIXED0.read();
        let cr4_fixed1 = Msr::IA32_VMX_CR4_FIXED1.read();
        let cr4_vmx = vmx_fixed_control_value(cr4, cr4_fixed0, cr4_fixed1);

        if !is_vmx_fixed_control_value_valid(cr0_vmx, cr0_fixed0, cr0_fixed1) {
            return x86_err!(BadState, "host CR0 is not valid in VMX operation");
        }
        if !is_vmx_fixed_control_value_valid(cr4_vmx, cr4_fixed0, cr4_fixed1) {
            return x86_err!(BadState, "host CR4 is not valid in VMX operation");
        }

        // Get VMCS revision identifier in IA32_VMX_BASIC MSR.
        let vmx_basic = VmxBasic::read();
        if vmx_basic.region_size as usize > PAGE_SIZE {
            return x86_err!(
                Unsupported,
                format_args!(
                    "unsupported VMX region size: {} bytes",
                    vmx_basic.region_size
                )
            );
        }
        if vmx_basic.mem_type != VmxBasic::VMX_MEMORY_TYPE_WRITE_BACK {
            return x86_err!(
                Unsupported,
                format_args!("unsupported VMX memory type: {}", vmx_basic.mem_type)
            );
        }
        if vmx_basic.is_32bit_address {
            return x86_err!(Unsupported, "unsupported 32-bit VMX physical address width");
        }
        if !vmx_basic.io_exit_info {
            return x86_err!(Unsupported, "VMX lacks I/O exit instruction info");
        }
        if !vmx_basic.vmx_flex_controls {
            return x86_err!(Unsupported, "VMX lacks flexible controls");
        }
        self.vmcs_revision_id = vmx_basic.revision_id;
        self.vmx_region = VmxRegion::<H>::new(self.vmcs_revision_id, false)?;

        unsafe {
            Cr0::write_raw(cr0_vmx);
            Cr4::write_raw(cr4_vmx);
            // Execute VMXON.
            vmx::vmxon(self.vmx_region.phys_addr().as_usize() as _).map_err(|err| {
                x86_err_type!(
                    BadState,
                    format_args!("VMX instruction vmxon failed: {:?}", err)
                )
            })?;
        }
        info!("[AxVM] succeeded to turn on VMX.");

        Ok(())
    }

    pub fn hardware_disable(&mut self) -> X86VcpuResult {
        if !self.is_enabled() {
            return x86_err!(BadState, "VMX is not enabled");
        }

        unsafe {
            // Execute VMXOFF.
            vmx::vmxoff().map_err(|err| {
                x86_err_type!(
                    BadState,
                    format_args!("VMX instruction vmxoff failed: {:?}", err)
                )
            })?;
            // Remove VMXE bit in CR4.
            Cr4::update(|cr4| cr4.remove(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS));
        };
        info!("[AxVM] succeeded to turn off VMX.");

        self.vmx_region = unsafe { VmxRegion::<H>::uninit() };
        Ok(())
    }
}

fn vmx_fixed_control_value(value: u64, fixed0: u64, fixed1: u64) -> u64 {
    (value | fixed0) & fixed1
}

fn is_vmx_fixed_control_value_valid(value: u64, fixed0: u64, fixed1: u64) -> bool {
    (value & fixed0) == fixed0 && (value & !fixed1) == 0
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec::Vec};

    use super::*;
    use crate::test_utils::mock::MockMmHal;

    type TestVmxPerCpuState = VmxPerCpuState<MockMmHal>;

    #[test]
    fn test_vmx_per_cpu_state_new() {
        MockMmHal::reset(); // Reset before test
        let result = TestVmxPerCpuState::new(0);
        assert!(result.is_ok());

        let state = result.unwrap();
        assert_eq!(state.vmcs_revision_id, 0);
    }

    #[test]
    fn test_vmx_per_cpu_state_default_values() {
        MockMmHal::reset(); // Reset before test
        let state = TestVmxPerCpuState::new(0).unwrap();

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
            let state = TestVmxPerCpuState::new(cpu_id).unwrap();
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
        let state = TestVmxPerCpuState::new(0).unwrap();

        // Test that Debug trait is implemented and doesn't panic
        let debug_str = format!("{:?}", state);
        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_vmx_per_cpu_state_size() {
        use core::mem;

        // Test that the struct has a reasonable size
        let size = mem::size_of::<TestVmxPerCpuState>();

        // Should be larger than just the u32 field due to the VmxRegion
        assert!(size > 4);

        // But shouldn't be excessively large (this is a sanity check)
        assert!(size < 1024);
    }
}
