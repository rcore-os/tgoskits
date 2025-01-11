//! Emulated Local APIC.
#![no_std]

extern crate alloc;

#[macro_use]
extern crate log;

mod consts;
mod lvt;
mod regs;
mod vlapic;

use alloc::boxed::Box;

use axerrno::AxResult;
use memory_addr::{AddrRange, PAGE_SIZE_4K};

use axaddrspace::device::{AccessWidth, SysRegAddr, SysRegAddrRange};
use axaddrspace::{AxMmHal, GuestPhysAddr, HostPhysAddr, HostVirtAddr};
use axdevice_base::{BaseDeviceOps, DeviceRWContext, EmuDeviceType, InterruptInjector};

use crate::consts::x2apic::x2apic_msr_access_reg;
use crate::consts::xapic::xapic_mmio_access_reg_offset;
use crate::vlapic::VirtualApicRegs;

#[repr(align(4096))]
struct APICAccessPage([u8; PAGE_SIZE_4K]);

static VIRTUAL_APIC_ACCESS_PAGE: APICAccessPage = APICAccessPage([0; PAGE_SIZE_4K]);

/// A emulated local APIC device.
pub struct EmulatedLocalApic<H: AxMmHal> {
    vlapic_regs: VirtualApicRegs<H>,
}

impl<H: AxMmHal> EmulatedLocalApic<H> {
    /// Create a new `EmulatedLocalApic`.
    pub fn new() -> Self {
        EmulatedLocalApic {
            vlapic_regs: VirtualApicRegs::new(),
        }
    }
}

impl<H: AxMmHal> EmulatedLocalApic<H> {
    /// APIC-access address (64 bits).
    /// This field contains the physical address of the 4-KByte APIC-access page.
    /// If the “virtualize APIC accesses” VM-execution control is 1,
    /// access to this page may cause VM exits or be virtualized by the processor.
    /// See Section 30.4.
    pub fn virtual_apic_access_addr() -> HostPhysAddr {
        H::virt_to_phys(HostVirtAddr::from_usize(
            VIRTUAL_APIC_ACCESS_PAGE.0.as_ptr() as usize,
        ))
    }

    /// Virtual-APIC address (64 bits).
    /// This field contains the physical address of the 4-KByte virtual-APIC page.
    /// The processor uses the virtual-APIC page to virtualize certain accesses to APIC registers and to manage virtual interrupts;
    /// see Chapter 30.
    pub fn virtual_apic_page_addr(&self) -> HostPhysAddr {
        self.vlapic_regs.virtual_apic_page_addr()
    }
}

impl<H: AxMmHal> BaseDeviceOps<AddrRange<GuestPhysAddr>> for EmulatedLocalApic<H> {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::EmuDeviceTInterruptController
    }

    fn address_range(&self) -> AddrRange<GuestPhysAddr> {
        use crate::consts::xapic::{APIC_MMIO_SIZE, DEFAULT_APIC_BASE};
        AddrRange::new(
            GuestPhysAddr::from_usize(DEFAULT_APIC_BASE),
            GuestPhysAddr::from_usize(DEFAULT_APIC_BASE + APIC_MMIO_SIZE),
        )
    }

    fn handle_read(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult<usize> {
        debug!(
            "EmulatedLocalApic::handle_read: addr={:?}, width={:?}, context={:?}",
            addr, width, context.vcpu_id
        );
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.vlapic_regs.handle_read(reg_off, width, context)
    }

    fn handle_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        context: DeviceRWContext,
    ) -> AxResult {
        debug!(
            "EmulatedLocalApic::handle_write: addr={:?}, width={:?}, val={:#x}, context={:?}",
            addr, width, val, context.vcpu_id
        );
        let reg_off = xapic_mmio_access_reg_offset(addr);
        self.vlapic_regs.handle_write(reg_off, width, context)
    }

    fn set_interrupt_injector(&mut self, _injector: Box<InterruptInjector>) {
        todo!()
    }
}

impl<H: AxMmHal> BaseDeviceOps<SysRegAddrRange> for EmulatedLocalApic<H> {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::EmuDeviceTInterruptController
    }

    fn address_range(&self) -> SysRegAddrRange {
        use crate::consts::x2apic::{X2APIC_MSE_REG_BASE, X2APIC_MSE_REG_SIZE};
        SysRegAddrRange::new(
            SysRegAddr(X2APIC_MSE_REG_BASE),
            SysRegAddr(X2APIC_MSE_REG_BASE + X2APIC_MSE_REG_SIZE),
        )
    }

    fn handle_read(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult<usize> {
        debug!(
            "EmulatedLocalApic::handle_read: addr={:?}, width={:?}, context={:?}",
            addr, width, context.vcpu_id
        );
        let reg_off = x2apic_msr_access_reg(addr);
        self.vlapic_regs.handle_read(reg_off, width, context)
    }

    fn handle_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
        context: DeviceRWContext,
    ) -> AxResult {
        debug!(
            "EmulatedLocalApic::handle_write: addr={:?}, width={:?}, val={:#x}, context={:?}",
            addr, width, val, context.vcpu_id
        );
        let reg_off = x2apic_msr_access_reg(addr);
        self.vlapic_regs.handle_write(reg_off, width, context)
    }

    fn set_interrupt_injector(&mut self, _injector: Box<InterruptInjector>) {
        todo!()
    }
}
