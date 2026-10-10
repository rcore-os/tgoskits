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
#[cfg(test)]
mod host_lock_provider;
#[cfg(test)]
mod lock;
mod pit;
mod regs;
mod timer;
mod timer_registration;
mod types;
mod utils;
mod vioapic;
mod vlapic;
mod vpic;

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
    /// The vLAPIC state is owned by exactly one vCPU task.
    ///
    /// Keeping the backend as a direct field makes that ownership visible in
    /// the Rust API: operations that change guest state require `&mut self`,
    /// so a caller cannot access the same vLAPIC concurrently through shared
    /// references or an interior-mutable escape hatch.
    vlapic_regs: VirtualApicRegs<H>,
}

pub use self::{
    host::{X86VlapicHostOps, X86VlapicRuntimeOps},
    pit::EmulatedPit,
    types::{
        X86AccessWidth, X86GuestPhysAddr, X86GuestPhysAddrRange, X86HostPhysAddr, X86HostVirtAddr,
        X86InterruptVector, X86MsrAddr, X86MsrAddrRange, X86Port, X86PortRange, X86TimerAction,
        X86TimerCallback, X86VcpuId, X86VlapicError, X86VlapicResult, X86VmId,
    },
    vioapic::{EmulatedIoApic, IoApicCore, IoApicEoi, IoApicInterrupt, IoApicOwner},
    vpic::{EmulatedPic, PicCore, PicInterruptClaim, PicOwner},
};

impl<H: host::X86VlapicHostOps> EmulatedLocalApic<H> {
    /// Create a new `EmulatedLocalApic`.
    pub fn new(runtime: H::Runtime, vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self {
        EmulatedLocalApic {
            vlapic_regs: VirtualApicRegs::new(runtime, vm_id, vcpu_id),
        }
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
        self.vlapic_regs.virtual_apic_page_addr()
    }

    /// Returns the current IA32_APIC_BASE MSR value.
    pub fn apic_base(&self) -> u64 {
        self.vlapic_regs.apic_base()
    }

    /// Sets the IA32_APIC_BASE MSR value.
    pub fn set_apic_base(&mut self, value: u64) -> X86VlapicResult {
        self.vlapic_regs.set_apic_base(value)
    }

    /// Record that the local APIC accepted an interrupt.
    pub fn accept_interrupt(&mut self, vector: u8, level_triggered: bool) {
        self.vlapic_regs.accept_interrupt(vector, level_triggered);
    }

    /// Returns whether the local APIC priority permits accepting `vector`.
    pub fn can_accept_interrupt(&self, vector: u8) -> bool {
        self.vlapic_regs.can_accept_interrupt(vector)
    }

    /// Returns whether the local APIC timer has an edge awaiting vCPU entry.
    pub fn has_pending_timer_interrupt(&self) -> bool {
        self.vlapic_regs.has_pending_timer_interrupt()
    }

    /// Coalesces expired local APIC timer periods into one pending vector.
    pub fn take_pending_timer_interrupt(&mut self) -> Option<u8> {
        self.vlapic_regs.take_pending_timer_interrupt()
    }

    /// Quiesces the local APIC timer for a task-side VM suspend while retaining
    /// the guest registers, canonical deadline and pending edge.
    pub fn suspend_timer(&mut self) -> X86VlapicResult {
        self.vlapic_regs.suspend_timer()
    }

    /// Reinstalls the local APIC timer quiesced by [`Self::suspend_timer`].
    pub fn resume_timer(&mut self) -> X86VlapicResult {
        self.vlapic_regs.resume_timer()
    }

    /// Cancels the local APIC timer and retires its guest-visible state.
    pub fn stop_timer(&mut self) -> X86VlapicResult {
        self.vlapic_regs.stop_timer()
    }

    /// Process a guest EOI and return the vector that needs an IO APIC EOI broadcast.
    pub fn handle_eoi(&mut self) -> Option<u8> {
        self.vlapic_regs.handle_eoi()
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
        self.vlapic_regs.handle_read(reg_off, width)
    }

    /// Handles an xAPIC MMIO write.
    pub fn handle_mmio_write(
        &mut self,
        addr: X86GuestPhysAddr,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        debug!(
            "EmulatedLocalApic::handle_mmio_write: addr={addr:?}, width={width:?}, val={val:#x}"
        );
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.vlapic_regs.handle_write(reg_off, val, width)
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
        self.vlapic_regs.handle_read(reg_off, width)
    }

    /// Handles an x2APIC MSR write.
    pub fn handle_msr_write(
        &mut self,
        addr: X86MsrAddr,
        width: X86AccessWidth,
        val: usize,
    ) -> X86VlapicResult {
        debug!("EmulatedLocalApic::handle_msr_write: addr={addr:?}, width={width:?}, val={val:#x}");
        let reg_off = x2apic_msr_access_reg(addr);
        self.vlapic_regs.handle_write(reg_off, val, width)
    }
}
