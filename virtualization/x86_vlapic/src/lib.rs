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
mod lock;
mod pit;
mod regs;
mod serial;
mod timer;
mod types;
mod utils;
mod vioapic;
mod vlapic;

use core::{cell::UnsafeCell, marker::PhantomData};

use crate::{
    consts::{x2apic::x2apic_msr_access_reg, xapic::xapic_mmio_access_reg_offset},
    host::X86_PAGE_SIZE_4K,
    vlapic::VirtualApicRegs,
};

#[repr(align(4096))]
struct APICAccessPage([u8; X86_PAGE_SIZE_4K]);

static VIRTUAL_APIC_ACCESS_PAGE: APICAccessPage = APICAccessPage([0; X86_PAGE_SIZE_4K]);

/// A emulated local APIC device.
pub struct EmulatedLocalApic<H: host::X86VlapicHostOps> {
    vlapic_regs: UnsafeCell<VirtualApicRegs<H>>,
    _host: PhantomData<fn() -> H>,
}

pub use self::{
    host::X86VlapicHostOps,
    pit::EmulatedPit,
    serial::{EmulatedSerialPort, X86SerialBackend},
    types::{
        X86AccessWidth, X86GuestPhysAddr, X86GuestPhysAddrRange, X86HostPhysAddr, X86HostVirtAddr,
        X86InterruptVector, X86MsrAddr, X86MsrAddrRange, X86Port, X86PortRange, X86TimerCallback,
        X86VcpuId, X86VlapicError, X86VlapicResult, X86VmId,
    },
    vioapic::{EmulatedIoApic, IoApicEoi, IoApicInterrupt},
};

impl<H: host::X86VlapicHostOps> EmulatedLocalApic<H> {
    /// Create a new `EmulatedLocalApic`.
    pub fn new(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self {
        EmulatedLocalApic {
            vlapic_regs: UnsafeCell::new(VirtualApicRegs::new(vm_id, vcpu_id)),
            _host: PhantomData,
        }
    }

    fn get_vlapic_regs(&self) -> &VirtualApicRegs<H> {
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
    fn get_mut_vlapic_regs(&self) -> &mut VirtualApicRegs<H> {
        unsafe { &mut *self.vlapic_regs.get() }
    }
}

impl<H: host::X86VlapicHostOps> EmulatedLocalApic<H> {
    /// APIC-access address (64 bits).
    /// This field contains the physical address of the 4-KByte APIC-access page.
    /// If the “virtualize APIC accesses” VM-execution control is 1,
    /// access to this page may cause VM exits or be virtualized by the processor.
    /// See Section 30.4.
    pub fn virtual_apic_access_addr() -> X86HostPhysAddr {
        host::virt_to_phys::<H>(X86HostVirtAddr::from_usize(
            VIRTUAL_APIC_ACCESS_PAGE.0.as_ptr() as usize,
        ))
    }

    /// Virtual-APIC address (64 bits).
    /// This field contains the physical address of the 4-KByte virtual-APIC page.
    /// The processor uses the virtual-APIC page to virtualize certain accesses to APIC registers and to manage virtual interrupts;
    /// see Chapter 30.
    pub fn virtual_apic_page_addr(&self) -> X86HostPhysAddr {
        self.get_vlapic_regs().virtual_apic_page_addr()
    }

    /// Returns the current IA32_APIC_BASE MSR value.
    pub fn apic_base(&self) -> u64 {
        self.get_vlapic_regs().apic_base()
    }

    /// Sets the IA32_APIC_BASE MSR value.
    pub fn set_apic_base(&self, value: u64) -> X86VlapicResult {
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

    /// Returns the xAPIC MMIO range.
    pub fn mmio_address_range(&self) -> X86GuestPhysAddrRange {
        use crate::consts::xapic::{APIC_MMIO_SIZE, DEFAULT_APIC_BASE};
        X86GuestPhysAddrRange::new(
            X86GuestPhysAddr::from_usize(DEFAULT_APIC_BASE),
            X86GuestPhysAddr::from_usize(DEFAULT_APIC_BASE + APIC_MMIO_SIZE),
        )
    }

    /// Handles an xAPIC MMIO read.
    pub fn handle_mmio_read(
        &self,
        addr: X86GuestPhysAddr,
        width: X86AccessWidth,
    ) -> X86VlapicResult<usize> {
        debug!("EmulatedLocalApic::handle_mmio_read: addr={addr:?}, width={width:?}");
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.get_vlapic_regs().handle_read(reg_off, width)
    }

    /// Handles an xAPIC MMIO write.
    pub fn handle_mmio_write(
        &self,
        addr: X86GuestPhysAddr,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        debug!(
            "EmulatedLocalApic::handle_mmio_write: addr={addr:?}, width={width:?}, val={val:#x}"
        );
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.get_mut_vlapic_regs().handle_write(reg_off, val, width)
    }

    /// Returns the x2APIC MSR range.
    pub fn msr_address_range(&self) -> X86MsrAddrRange {
        use crate::consts::x2apic::{X2APIC_MSE_REG_BASE, X2APIC_MSE_REG_SIZE};
        X86MsrAddrRange::new(
            X86MsrAddr::new(X2APIC_MSE_REG_BASE),
            X86MsrAddr::new(X2APIC_MSE_REG_BASE + X2APIC_MSE_REG_SIZE),
        )
    }

    /// Handles an x2APIC MSR read.
    pub fn handle_msr_read(
        &self,
        addr: X86MsrAddr,
        width: X86AccessWidth,
    ) -> X86VlapicResult<usize> {
        debug!("EmulatedLocalApic::handle_msr_read: addr={addr:?}, width={width:?}");
        let reg_off = x2apic_msr_access_reg(addr);
        self.get_vlapic_regs().handle_read(reg_off, width)
    }

    /// Handles an x2APIC MSR write.
    pub fn handle_msr_write(
        &self,
        addr: X86MsrAddr,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        debug!("EmulatedLocalApic::handle_msr_write: addr={addr:?}, width={width:?}, val={val:#x}");
        let reg_off = x2apic_msr_access_reg(addr);
        self.get_mut_vlapic_regs().handle_write(reg_off, val, width)
    }
}
