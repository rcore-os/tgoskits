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

//! Captures the host machine bank used by VM exit.

use super::{VmxControls, VmxEntryContext, fields::*};
use crate::virtualization::{ControlMemory, VirtualizationError};

impl<M: ControlMemory> VmxControls<M> {
    /// Captures this CPU's host register bank into the bound VMCS.
    ///
    /// # Safety
    /// The caller must own the binding CPU at ring 0 with IRQs disabled and
    /// retain its address space, GDT, TSS and entry context through guest entry.
    /// The installed GDT must contain a valid, mapped 64-bit TSS descriptor.
    /// The entry context must not move until the VMCS is retired or recaptured.
    pub unsafe fn capture_host(
        &mut self,
        entry: &VmxEntryContext,
    ) -> Result<(), VirtualizationError> {
        use x86::{
            dtables::{self, DescriptorTablePointer},
            segmentation,
        };
        use x86_64::registers::{
            control::{Cr0, Cr3, Cr4},
            model_specific::Msr,
        };
        // SAFETY: all selected registers exist in a VMX-capable 64-bit host;
        // the caller retains its descriptor mappings and CPU binding.
        unsafe {
            self.write(VmcsHost64::IA32_PAT, Msr::new(0x277).read())?;
            self.write(VmcsHost64::IA32_EFER, Msr::new(0xc000_0080).read())?;
            self.write(VmcsHostNW::CR0, Cr0::read_raw() as usize)?;
            self.write(
                VmcsHostNW::CR3,
                Cr3::read_raw().0.start_address().as_u64() as usize,
            )?;
            self.write(VmcsHostNW::CR4, Cr4::read_raw() as usize)?;
            self.write(VmcsHost16::ES_SELECTOR, segmentation::es().bits())?;
            self.write(VmcsHost16::CS_SELECTOR, segmentation::cs().bits())?;
            self.write(VmcsHost16::SS_SELECTOR, segmentation::ss().bits())?;
            self.write(VmcsHost16::DS_SELECTOR, segmentation::ds().bits())?;
            self.write(VmcsHost16::FS_SELECTOR, segmentation::fs().bits())?;
            self.write(VmcsHost16::GS_SELECTOR, segmentation::gs().bits())?;
            self.write(VmcsHostNW::FS_BASE, Msr::new(0xc000_0100).read() as usize)?;
            self.write(VmcsHostNW::GS_BASE, Msr::new(0xc000_0101).read() as usize)?;
            let tr = x86::task::tr();
            let mut gdt = DescriptorTablePointer::<u64>::default();
            let mut idt = DescriptorTablePointer::<u64>::default();
            dtables::sgdt(&mut gdt);
            dtables::sidt(&mut idt);
            let offset = usize::from(tr.index());
            if (offset + 2) * core::mem::size_of::<u64>() > usize::from(gdt.limit) + 1 {
                return Err(VirtualizationError::InvalidControlMemory);
            }
            let low = gdt.base.add(offset).read();
            let high = gdt.base.add(offset + 1).read();
            let base =
                ((low >> 16) & 0xffffff) | ((low >> 56) << 24) | ((high & 0xffff_ffff) << 32);
            self.write(VmcsHost16::TR_SELECTOR, tr.bits())?;
            self.write(VmcsHostNW::TR_BASE, base as usize)?;
            self.write(VmcsHostNW::GDTR_BASE, gdt.base as usize)?;
            self.write(VmcsHostNW::IDTR_BASE, idt.base as usize)?;
            self.write(VmcsHostNW::RIP, VmxEntryContext::exit_address().as_usize())?;
            self.write(VmcsHostNW::RSP, entry.host_stack_slot().as_usize())?;
            self.write(
                VmcsHostNW::IA32_SYSENTER_ESP,
                Msr::new(0x175).read() as usize,
            )?;
            self.write(
                VmcsHostNW::IA32_SYSENTER_EIP,
                Msr::new(0x176).read() as usize,
            )?;
            self.write(VmcsHost32::IA32_SYSENTER_CS, Msr::new(0x174).read() as u32)?;
        }
        Ok(())
    }
}
