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
mod timer;
mod timer_registration;
mod types;
mod utils;
mod vioapic;
mod vlapic;
mod vpic;

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
    types::{
        X86AccessWidth, X86GuestPhysAddr, X86GuestPhysAddrRange, X86HostPhysAddr, X86HostVirtAddr,
        X86InterruptVector, X86MsrAddr, X86MsrAddrRange, X86Port, X86PortRange, X86TimerAction,
        X86TimerCallback, X86VcpuId, X86VlapicError, X86VlapicResult, X86VmId,
    },
    vioapic::{EmulatedIoApic, IoApicEoi, IoApicInterrupt},
    vpic::{EmulatedPic, PicInterruptClaim},
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

    /// Returns whether a fixed interrupt passes the local APIC priority.
    pub fn can_accept_interrupt(&self, vector: u8) -> bool {
        let ppr = self.processor_priority();
        self.get_vlapic_regs()
            .can_accept_interrupt_with_priority(vector, ppr)
    }

    /// Returns the current local APIC processor-priority register value.
    pub fn processor_priority(&self) -> u8 {
        self.get_vlapic_regs().processor_priority()
    }

    /// Returns whether the local APIC timer has an edge awaiting vCPU entry.
    pub fn has_pending_timer_interrupt(&self) -> bool {
        self.get_vlapic_regs().has_pending_timer_interrupt()
    }

    /// Coalesces expired local APIC timer periods into one pending vector.
    pub fn take_pending_timer_interrupt(&self) -> Option<u8> {
        self.get_vlapic_regs().take_pending_timer_interrupt()
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

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    #[repr(align(4096))]
    struct TestFrame {
        _bytes: [u8; host::X86_PAGE_SIZE_4K],
    }

    struct TestHost;

    impl host::X86VlapicHostOps for TestHost {
        type TimerHandle = ();

        fn alloc_frame() -> Option<X86HostPhysAddr> {
            Some(X86HostPhysAddr::from_usize(
                Box::into_raw(Box::new(TestFrame {
                    _bytes: [0; host::X86_PAGE_SIZE_4K],
                })) as usize,
            ))
        }

        fn dealloc_frame(paddr: X86HostPhysAddr) {
            // SAFETY: `paddr` was returned by `alloc_frame`, and the owning
            // `PhysFrame` is the only live owner when this callback runs.
            unsafe {
                drop(Box::from_raw(paddr.as_mut_ptr::<TestFrame>()));
            }
        }

        fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
            X86HostVirtAddr::from_usize(paddr.as_usize())
        }

        fn virt_to_phys(vaddr: X86HostVirtAddr) -> X86HostPhysAddr {
            X86HostPhysAddr::from_usize(vaddr.as_usize())
        }

        fn current_time_nanos() -> u64 {
            0
        }

        fn register_timer(
            _deadline_nanos: u64,
            _callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            Err(X86VlapicError::TimerUnavailable)
        }

        unsafe fn register_hard_timer(
            _deadline_nanos: u64,
            _callback: X86TimerCallback,
        ) -> X86VlapicResult<Self::TimerHandle> {
            Err(X86VlapicError::TimerUnavailable)
        }

        fn cancel_timer(_handle: Self::TimerHandle) -> X86VlapicResult {
            Ok(())
        }

        fn current_vm_id() -> X86VmId {
            1
        }

        fn current_vm_vcpu_num() -> usize {
            1
        }

        fn current_vm_active_vcpus() -> usize {
            1
        }

        fn active_vcpus(_vm_id: X86VmId) -> Option<usize> {
            Some(1)
        }

        fn inject_interrupt(
            _vm_id: X86VmId,
            _vcpu_id: X86VcpuId,
            _vector: X86InterruptVector,
        ) -> X86VlapicResult {
            Ok(())
        }
    }

    #[test]
    fn same_priority_interrupt_is_blocked_until_eoi() {
        let lapic = EmulatedLocalApic::<TestHost>::new(1, 0);

        assert!(lapic.can_accept_interrupt(0x68));
        lapic.accept_interrupt(0x68, false);

        assert!(!lapic.can_accept_interrupt(0x68));
        assert!(!lapic.can_accept_interrupt(0x21));
        assert!(lapic.can_accept_interrupt(0x71));

        lapic.handle_eoi();
        assert!(lapic.can_accept_interrupt(0x68));
    }
}
