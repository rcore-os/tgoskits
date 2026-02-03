//! Emulated Local APIC.
#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

#[macro_use]
extern crate log;

mod consts;
mod regs;
mod timer;
mod utils;
mod vlapic;

use core::cell::UnsafeCell;

use axerrno::AxResult;
use axvisor_api::{
    memory,
    vmm::{VCpuId, VMId},
};
use memory_addr::{AddrRange, PAGE_SIZE_4K};

use axaddrspace::{
    GuestPhysAddr, HostPhysAddr, HostVirtAddr,
    device::{AccessWidth, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

use crate::consts::x2apic::x2apic_msr_access_reg;
use crate::consts::xapic::xapic_mmio_access_reg_offset;
use crate::vlapic::VirtualApicRegs;

#[repr(align(4096))]
struct APICAccessPage([u8; PAGE_SIZE_4K]);

static VIRTUAL_APIC_ACCESS_PAGE: APICAccessPage = APICAccessPage([0; PAGE_SIZE_4K]);

/// A emulated local APIC device.
pub struct EmulatedLocalApic {
    vlapic_regs: UnsafeCell<VirtualApicRegs>,
}

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

    #[allow(clippy::mut_from_ref)] // SAFETY: get_mut_vlapic_regs is never called concurrently.
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
        memory::virt_to_phys(HostVirtAddr::from_usize(
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
