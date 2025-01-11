use core::ptr::NonNull;

use axerrno::AxResult;
use tock_registers::interfaces::Readable;

use axaddrspace::device::AccessWidth;
use axaddrspace::{AxMmHal, HostPhysAddr, PhysFrame};
use axdevice_base::DeviceRWContext;

use crate::consts::{ApicRegOffset, RESET_SPURIOUS_INTERRUPT_VECTOR};
use crate::lvt::LocalVectorTable;
use crate::regs::{LocalAPICRegs, SpuriousInterruptVectorRegisterLocal};

/// Virtual-APIC Registers.
pub struct VirtualApicRegs<H: AxMmHal> {
    /// The virtual-APIC page is a 4-KByte region of memory
    /// that the processor uses to virtualize certain accesses to APIC registers and to manage virtual interrupts.
    /// The physical address of the virtual-APIC page is the virtual-APIC address,
    /// a 64-bit VM-execution control field in the VMCS (see Section 25.6.8).
    virtual_lapic: NonNull<LocalAPICRegs>,
    /// Copies of some registers in the virtual APIC page,
    /// to be able to detect what changed (e.g. svr_last)
    svr_last: SpuriousInterruptVectorRegisterLocal,
    /// Copies of some registers in the virtual APIC page,
    /// to maintain a coherent snapshot of the register (e.g. lvt_last)
    lvt_last: LocalVectorTable,
    apic_page: PhysFrame<H>,
}

impl<H: AxMmHal> VirtualApicRegs<H> {
    /// Create new virtual-APIC registers by allocating a 4-KByte page for the virtual-APIC page.
    pub fn new() -> Self {
        let apic_frame = PhysFrame::alloc_zero().expect("allocate virtual-APIC page failed");
        Self {
            virtual_lapic: NonNull::new(apic_frame.as_mut_ptr().cast()).unwrap(),
            apic_page: apic_frame,
            svr_last: SpuriousInterruptVectorRegisterLocal::new(RESET_SPURIOUS_INTERRUPT_VECTOR),
            lvt_last: LocalVectorTable::default(),
        }
    }

    const fn regs(&self) -> &LocalAPICRegs {
        unsafe { self.virtual_lapic.as_ref() }
    }

    /// Virtual-APIC address (64 bits).
    /// This field contains the physical address of the 4-KByte virtual-APIC page.
    /// The processor uses the virtual-APIC page to virtualize certain accesses to APIC registers and to manage virtual interrupts;
    /// see Chapter 30.
    pub fn virtual_apic_page_addr(&self) -> HostPhysAddr {
        self.apic_page.start_paddr()
    }
}

impl<H: AxMmHal> Drop for VirtualApicRegs<H> {
    fn drop(&mut self) {
        H::dealloc_frame(self.apic_page.start_paddr());
    }
}

impl<H: AxMmHal> VirtualApicRegs<H> {
    pub fn handle_read(
        &self,
        offset: ApicRegOffset,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult<usize> {
        let mut value: usize = 0;
        match offset {
            ApicRegOffset::ID => {
                value = self.regs().ID.get() as _;
                debug!("[VLAPIC] read APIC ID register: {:#010X}", value);
            }
            ApicRegOffset::Version => {
                value = self.regs().VERSION.get() as _;
                debug!("[VLAPIC] read APIC Version register: {:#010X}", value);
            }
            ApicRegOffset::TPR => {
                value = self.regs().TPR.get() as _;
                debug!("[VLAPIC] read TPR register: {:#010X}", value);
            }
            ApicRegOffset::PPR => {
                value = self.regs().PPR.get() as _;
                debug!("[VLAPIC] read PPR register: {:#010X}", value);
            }
            ApicRegOffset::EOI => {
                // value = self.regs().EOI.get() as _;
                warn!("[VLAPIC] read EOI register: {:#010X}", value);
            }
            ApicRegOffset::LDR => {
                value = self.regs().LDR.get() as _;
                debug!("[VLAPIC] read LDR register: {:#010X}", value);
            }
            ApicRegOffset::DFR => {
                value = self.regs().DFR.get() as _;
                debug!("[VLAPIC] read DFR register: {:#010X}", value);
            }
            ApicRegOffset::SIVR => {
                value = self.regs().SVR.get() as _;
                debug!("[VLAPIC] read SVR register: {:#010X}", value);
            }
            ApicRegOffset::ISR(index) => {
                value = self.regs().ISR[index.as_usize()].get() as _;
                debug!("[VLAPIC] read ISR[{}] register: {:#010X}", index, value);
            }
            ApicRegOffset::TMR(index) => {
                value = self.regs().TMR[index.as_usize()].get() as _;
                debug!("[VLAPIC] read TMR[{}] register: {:#010X}", index, value);
            }
            ApicRegOffset::IRR(index) => {
                value = self.regs().IRR[index.as_usize()].get() as _;
                debug!("[VLAPIC] read IRR[{}] register: {:#010X}", index, value);
            }
            ApicRegOffset::ESR => {
                value = self.regs().ESR.get() as _;
                debug!("[VLAPIC] read ESR register: {:#010X}", value);
            }
            ApicRegOffset::ICRLow => {
                value = self.regs().ICR_LO.get() as _;
                debug!("[VLAPIC] read ICR_LOW register: {:#010X}", value);
                if width == AccessWidth::Qword {
                    let icr_hi = self.regs().ICR_HI.get() as usize;
                    value |= icr_hi << 32;
                    debug!("[VLAPIC] read ICR register: {:#018X}", value);
                }
            }
            ApicRegOffset::ICRHi => {
                value = self.regs().ICR_HI.get() as _;
                debug!("[VLAPIC] read ICR_HI register: {:#010X}", value);
            }
            ApicRegOffset::LvtCMCI => {
                value = self.lvt_last.lvt_cmci.get() as _;
                debug!("[VLAPIC] read LVT_CMCI register: {:#010X}", value);
            }
            ApicRegOffset::LvtTimer => {
                value = self.lvt_last.lvt_timer.get() as _;
                debug!("[VLAPIC] read LVT_TIMER register: {:#010X}", value);
            }
            ApicRegOffset::LvtThermal => {
                value = self.lvt_last.lvt_thermal.get() as _;
                debug!("[VLAPIC] read LvtThermal register: {:#010X}", value);
            }
            ApicRegOffset::LvtPmc => {
                value = self.lvt_last.lvt_perf_count.get() as _;
                debug!("[VLAPIC] read LvtPmi register: {:#010X}", value);
            }
            ApicRegOffset::LvtLint0 => {
                value = self.lvt_last.lvt_lint0.get() as _;
                debug!("[VLAPIC] read LvtLint0 register: {:#010X}", value);
            }
            ApicRegOffset::LvtLint1 => {
                value = self.lvt_last.lvt_lint1.get() as _;
                debug!("[VLAPIC] read LvtLint1 register: {:#010X}", value);
            }
            ApicRegOffset::LvtErr => {
                value = self.lvt_last.lvt_err.get() as _;
                debug!("[VLAPIC] read LvtErr register: {:#010X}", value);
            }
            ApicRegOffset::TimerInitCount => {
                value = self.regs().ICR_TIMER.get() as _;
                debug!("[VLAPIC] read TimerInitCount register: {:#010X}", value);
            }
            ApicRegOffset::TimerCurCount => {
                value = self.regs().CCR_TIMER.get() as _;
                debug!("[VLAPIC] read TimerCurCount register: {:#010X}", value);
            }
            ApicRegOffset::TimerDivConf => {
                value = self.regs().DCR_TIMER.get() as _;
                debug!("[VLAPIC] read TimerDivConf register: {:#010X}", value);
            }
            _ => {
                warn!("[VLAPIC] read unknown APIC register: {:?}", offset);
            }
        }
        Ok(value)
    }

    pub fn handle_write(
        &self,
        offset: ApicRegOffset,
        width: AccessWidth,
        context: DeviceRWContext,
    ) -> AxResult {
        Ok(())
    }
}
