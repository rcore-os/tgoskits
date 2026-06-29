use super::vector::{APIC_IPI_VECTOR, lapic_ipi_irq_id};
use crate::irq::{IrqError, IrqId};

const LAPIC_REG_EOI: u32 = 0x0b0;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
pub(super) const ICR_FIXED_BASE: u32 = 0x0000_4000;
pub(super) const ICR_DEST_SELF: u32 = 0x0004_0000;
pub(super) const ICR_DEST_ALL_EXCLUDING_SELF: u32 = 0x000c_0000;
const IPI_DELIVERY_WAIT_SPINS: usize = 1_000_000;

const IA32_APIC_BASE_MSR: u32 = 0x1b;
const IA32_APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10;
const IA32_X2APIC_EOI: u32 = 0x80b;
const IA32_X2APIC_ICR: u32 = 0x830;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApicMode {
    XApic,
    X2Apic,
}

pub(super) fn eoi() {
    unsafe {
        match current_apic_mode() {
            ApicMode::X2Apic => x86::msr::wrmsr(IA32_X2APIC_EOI, 0),
            ApicMode::XApic => lapic_write(LAPIC_REG_EOI, 0),
        }
    }
}

pub(super) fn ipi_vector(irq: IrqId) -> Result<u8, IrqError> {
    if irq == lapic_ipi_irq_id() {
        Ok(APIC_IPI_VECTOR as u8)
    } else {
        Err(IrqError::InvalidIrq)
    }
}

fn current_apic_mode() -> ApicMode {
    let base = unsafe { x86::msr::rdmsr(IA32_APIC_BASE_MSR) };
    if base & IA32_APIC_BASE_X2APIC_ENABLE != 0 {
        ApicMode::X2Apic
    } else {
        ApicMode::XApic
    }
}

pub(super) fn xapic_destination(apic_id: u32) -> Result<u32, IrqError> {
    let dest = u8::try_from(apic_id).map_err(|_| IrqError::InvalidCpu)?;
    Ok(u32::from(dest) << 24)
}

pub(super) fn x2apic_icr(apic_id: u32, icr_low: u32) -> u64 {
    (u64::from(apic_id) << 32) | u64::from(icr_low)
}

pub(super) fn send_ipi_to_apic_id(apic_id: u32, icr_low: u32) -> Result<(), IrqError> {
    match current_apic_mode() {
        ApicMode::X2Apic => send_x2apic_ipi(x2apic_icr(apic_id, icr_low)),
        ApicMode::XApic => send_xapic_ipi(xapic_destination(apic_id)?, icr_low),
    }
}

pub(super) fn send_ipi(destination: u32, icr_low: u32) -> Result<(), IrqError> {
    match current_apic_mode() {
        ApicMode::X2Apic => send_x2apic_ipi(u64::from(icr_low)),
        ApicMode::XApic => send_xapic_ipi(destination, icr_low),
    }
}

fn send_xapic_ipi(destination: u32, icr_low: u32) -> Result<(), IrqError> {
    unsafe {
        lapic_write(LAPIC_REG_ICR_HIGH, destination);
        lapic_write(LAPIC_REG_ICR_LOW, icr_low);
    }
    wait_xapic_delivery()
}

fn send_x2apic_ipi(icr: u64) -> Result<(), IrqError> {
    unsafe {
        x86::msr::wrmsr(IA32_X2APIC_ICR, icr);
    }
    wait_x2apic_delivery()
}

fn wait_xapic_delivery() -> Result<(), IrqError> {
    for _ in 0..IPI_DELIVERY_WAIT_SPINS {
        if unsafe { lapic_read(LAPIC_REG_ICR_LOW) } & ICR_DELIVERY_PENDING == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(IrqError::Timeout)
}

fn wait_x2apic_delivery() -> Result<(), IrqError> {
    for _ in 0..IPI_DELIVERY_WAIT_SPINS {
        if unsafe { x86::msr::rdmsr(IA32_X2APIC_ICR) } & u64::from(ICR_DELIVERY_PENDING) == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(IrqError::Timeout)
}

unsafe fn lapic_read(offset: u32) -> u32 {
    let ptr = lapic_ptr(offset) as *const u32;
    unsafe { ptr.read_volatile() }
}

unsafe fn lapic_write(offset: u32, value: u32) {
    let ptr = lapic_ptr(offset);
    unsafe {
        ptr.write_volatile(value);
    }
}

fn lapic_ptr(offset: u32) -> *mut u32 {
    const IA32_APIC_BASE: u32 = 0x1b;
    const LAPIC_BASE_MASK: u64 = 0xffff_f000;
    let base = unsafe { x86::msr::rdmsr(IA32_APIC_BASE) & LAPIC_BASE_MASK } as usize;
    unsafe { someboot::mem::phys_to_virt(base).add(offset as usize) }.cast()
}
