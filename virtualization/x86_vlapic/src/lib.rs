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

//! Emulated Local APIC.
#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

#[macro_use]
extern crate log;

mod consts;
pub mod host;
mod pit;
mod regs;
mod serial;
mod timer;
mod utils;
mod vioapic;
mod vlapic;

use core::cell::UnsafeCell;

use ax_errno::AxResult;
use ax_memory_addr::{AddrRange, PAGE_SIZE_4K};
use axdevice_base::{AccessWidth, BaseDeviceOps, EmuDeviceType, SysRegAddr, SysRegAddrRange};
use axvm_types::{GuestPhysAddr, HostPhysAddr, HostVirtAddr, VCpuId, VMId};

use crate::{
    consts::{x2apic::x2apic_msr_access_reg, xapic::xapic_mmio_access_reg_offset},
    vlapic::VirtualApicRegs,
};

#[repr(align(4096))]
struct APICAccessPage([u8; PAGE_SIZE_4K]);

static VIRTUAL_APIC_ACCESS_PAGE: APICAccessPage = APICAccessPage([0; PAGE_SIZE_4K]);

/// A emulated local APIC device.
pub struct EmulatedLocalApic {
    vlapic_regs: UnsafeCell<VirtualApicRegs>,
}

pub use pit::EmulatedPit;
pub use serial::EmulatedSerialPort;
pub use vioapic::{EmulatedIoApic, IoApicEoi, IoApicInterrupt};

impl EmulatedLocalApic {
    /// Create a new `EmulatedLocalApic`.
    pub fn new(vm_id: VMId, vcpu_id: VCpuId) -> Self {
        EmulatedLocalApic {
            vlapic_regs: UnsafeCell::new(VirtualApicRegs::new(vm_id, vcpu_id)),
        }
    }

    fn get_vlapic_regs(&self) -> &VirtualApicRegs {
        unsafe { &*self.vlapic_regs.get() }
    }

    /// Returns mutable access to the virtual APIC register state.
    ///
    /// # Safety
    ///
    /// `vlapic_regs` is stored in an [`UnsafeCell`] because the vLAPIC MMIO/MSR
    /// handlers are exposed through shared device references. Callers must
    /// guarantee that no two execution contexts call this method, or otherwise
    /// mutate/read the same [`VirtualApicRegs`], concurrently. In the current
    /// Axvisor x86 path each `EmulatedLocalApic` is owned by one vCPU and vLAPIC
    /// register accesses are handled synchronously on that vCPU's run path; any
    /// cross-vCPU interrupt requests are funneled through the vCPU task instead
    /// of directly mutating another vCPU's local APIC registers.
    #[allow(clippy::mut_from_ref)]
    fn get_mut_vlapic_regs(&self) -> &mut VirtualApicRegs {
        unsafe { &mut *self.vlapic_regs.get() }
    }
}

impl EmulatedLocalApic {
    /// APIC-access address (64 bits).
    /// This field contains the physical address of the 4-KByte APIC-access page.
    /// If the “virtualize APIC accesses” VM-execution control is 1,
    /// access to this page may cause VM exits or be virtualized by the processor.
    /// See Section 30.4.
    pub fn virtual_apic_access_addr() -> HostPhysAddr {
        host::virt_to_phys(HostVirtAddr::from_usize(
            VIRTUAL_APIC_ACCESS_PAGE.0.as_ptr() as usize,
        ))
    }

    /// Virtual-APIC address (64 bits).
    /// This field contains the physical address of the 4-KByte virtual-APIC page.
    /// The processor uses the virtual-APIC page to virtualize certain accesses to APIC registers and to manage virtual interrupts;
    /// see Chapter 30.
    pub fn virtual_apic_page_addr(&self) -> HostPhysAddr {
        self.get_vlapic_regs().virtual_apic_page_addr()
    }

    /// Returns the current IA32_APIC_BASE MSR value.
    pub fn apic_base(&self) -> u64 {
        self.get_vlapic_regs().apic_base()
    }

    /// Sets the IA32_APIC_BASE MSR value.
    pub fn set_apic_base(&self, value: u64) -> AxResult {
        self.get_mut_vlapic_regs().set_apic_base(value)
    }

    /// Record that the local APIC accepted an interrupt.
    pub fn accept_interrupt(&self, vector: u8, level_triggered: bool) {
        self.get_mut_vlapic_regs()
            .accept_interrupt(vector, level_triggered);
    }

    /// Process a guest EOI and return the vector that needs an IO APIC EOI broadcast.
    pub fn handle_eoi(&self) -> Option<u8> {
        self.get_mut_vlapic_regs().handle_eoi()
    }
}

impl BaseDeviceOps<AddrRange<GuestPhysAddr>> for EmulatedLocalApic {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::InterruptController
    }

    fn address_range(&self) -> AddrRange<GuestPhysAddr> {
        use crate::consts::xapic::{APIC_MMIO_SIZE, DEFAULT_APIC_BASE};
        AddrRange::new(
            GuestPhysAddr::from_usize(DEFAULT_APIC_BASE),
            GuestPhysAddr::from_usize(DEFAULT_APIC_BASE + APIC_MMIO_SIZE),
        )
    }

    fn handle_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        debug!("EmulatedLocalApic::handle_read: addr={addr:?}, width={width:?}");
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.get_vlapic_regs().handle_read(reg_off, width)
    }

    fn handle_write(&self, addr: GuestPhysAddr, width: AccessWidth, val: usize) -> AxResult {
        debug!("EmulatedLocalApic::handle_write: addr={addr:?}, width={width:?}, val={val:#x}");
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.get_mut_vlapic_regs().handle_write(reg_off, val, width)
    }
}

impl BaseDeviceOps<SysRegAddrRange> for EmulatedLocalApic {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::InterruptController
    }

    fn address_range(&self) -> SysRegAddrRange {
        use crate::consts::x2apic::{X2APIC_MSE_REG_BASE, X2APIC_MSE_REG_SIZE};
        SysRegAddrRange::new(
            SysRegAddr(X2APIC_MSE_REG_BASE),
            SysRegAddr(X2APIC_MSE_REG_BASE + X2APIC_MSE_REG_SIZE),
        )
    }

    fn handle_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        debug!("EmulatedLocalApic::handle_read: addr={addr:?}, width={width:?}");
        let reg_off = x2apic_msr_access_reg(addr);
        self.get_vlapic_regs().handle_read(reg_off, width)
    }

    fn handle_write(&self, addr: SysRegAddr, width: AccessWidth, val: usize) -> AxResult {
        debug!("EmulatedLocalApic::handle_write: addr={addr:?}, width={width:?}, val={val:#x}");
        let reg_off = x2apic_msr_access_reg(addr);
        self.get_mut_vlapic_regs().handle_write(reg_off, val, width)
    }
}
