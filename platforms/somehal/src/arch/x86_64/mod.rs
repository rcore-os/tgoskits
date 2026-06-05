use alloc::vec::Vec;

use rdrive::probe::acpi::{AcpiIoApic, AcpiIrqPolarity, AcpiIrqTrigger};
use spin::Mutex;
use x2apic::ioapic::{IoApic, IrqFlags, IrqMode};

use crate::{common::PlatOp, irq::_handle_irq};

pub struct Plat;

const APIC_TIMER_VECTOR: usize = 0x20;
const IOAPIC_VECTOR_BASE: usize = 0x30;

const LAPIC_REG_EOI: u32 = 0x0b0;
const LAPIC_REG_ICR_LOW: u32 = 0x300;
const LAPIC_REG_ICR_HIGH: u32 = 0x310;
const ICR_DELIVERY_PENDING: u32 = 1 << 12;
const ICR_FIXED_BASE: u32 = 0x0000_4000;
const ICR_DEST_SELF: u32 = 0x0004_0000;
const ICR_DEST_ALL_EXCLUDING_SELF: u32 = 0x000c_0000;

static IOAPICS: Mutex<Vec<IoApicState>> = Mutex::new(Vec::new());

struct IoApicState {
    info: AcpiIoApic,
    ioapic: IoApic,
}

impl IoApicState {
    fn contains(&self, gsi: u32) -> bool {
        let start = self.info.gsi_base;
        let end = start.saturating_add(u32::from(self.info.redirection_entries));
        (start..end).contains(&gsi)
    }

    fn input_for(&self, gsi: u32) -> Option<u8> {
        let input = gsi.checked_sub(self.info.gsi_base)?;
        u8::try_from(input).ok()
    }
}

impl PlatOp for Plat {
    fn irq_set_enable(irq: rdrive::IrqId, enable: bool) {
        let raw = irq.raw();

        if raw == someboot::irq::systimer_irq().raw() {
            someboot::irq::irq_set_enable(someboot::irq::IrqId::new(raw), enable);
            return;
        }

        set_ioapic_vector_enable(raw, enable);
    }

    fn send_ipi(irq: rdrive::IrqId, target: crate::irq::IpiTarget) {
        let vector = irq.raw() as u8;

        unsafe {
            match target {
                crate::irq::IpiTarget::Current { .. } => {
                    send_lapic_ipi(0, ICR_FIXED_BASE | ICR_DEST_SELF | u32::from(vector))
                }
                crate::irq::IpiTarget::Other { cpu_id } => {
                    let Some(apic_id) = someboot::smp::cpu_idx_to_id(cpu_id) else {
                        warn!("failed to resolve CPU index {cpu_id} to APIC ID");
                        return;
                    };
                    send_lapic_ipi(raw_apic_id(apic_id), ICR_FIXED_BASE | u32::from(vector));
                }
                crate::irq::IpiTarget::AllExceptCurrent { .. } => {
                    send_lapic_ipi(
                        0,
                        ICR_FIXED_BASE | ICR_DEST_ALL_EXCLUDING_SELF | u32::from(vector),
                    );
                }
            }
        }
    }

    fn irq_handler() -> someboot::irq::IrqId {
        someboot::irq::systimer_irq()
    }

    fn irq_handler_with_raw(raw: usize) -> Option<someboot::irq::IrqId> {
        if raw == APIC_TIMER_VECTOR {
            _handle_irq(raw.into());
            lapic_eoi();
            return Some(someboot::irq::systimer_irq());
        }

        _handle_irq(raw.into());
        lapic_eoi();
        Some(someboot::irq::IrqId::new(raw))
    }

    fn systick_irq() -> rdrive::IrqId {
        someboot::irq::systimer_irq().raw().into()
    }

    fn secondary_init() {}

    fn secondary_init_intc(_cpu_idx: usize) {}

    fn secondary_init_systick() {}

    fn send_ipi_to_cpu(cpu_id: usize) {
        Self::send_ipi(
            APIC_TIMER_VECTOR.into(),
            crate::irq::IpiTarget::Other { cpu_id },
        );
    }
}

pub fn init_acpi_irq() {
    init_ioapics_from_acpi();
}

fn init_ioapics_from_acpi() {
    let Some(routing) = rdrive::probe::acpi::with_acpi(|system| system.routing().clone()) else {
        return;
    };

    let mut ioapics = IOAPICS.lock();
    if !ioapics.is_empty() {
        return;
    }

    for info in routing.io_apics().iter().copied() {
        let ioapic_base = someboot::mem::phys_to_virt(info.address as usize) as u64;
        let mut ioapic = unsafe { IoApic::new(ioapic_base) };
        let max_entry = unsafe { ioapic.max_table_entry() };
        let redirection_entries = max_entry.saturating_add(1);

        unsafe {
            ioapic.init(IOAPIC_VECTOR_BASE as u8);
            for input in 0..=max_entry {
                let mut entry = ioapic.table_entry(input);
                entry.set_flags(entry.flags() | IrqFlags::MASKED);
                ioapic.set_table_entry(input, entry);
            }
        }

        info!(
            "ACPI IOAPIC initialized: id={} base={:#x} gsi_base={} entries={}",
            info.id, info.address, info.gsi_base, redirection_entries
        );
        ioapics.push(IoApicState {
            info: AcpiIoApic {
                redirection_entries,
                ..info
            },
            ioapic,
        });
    }
}

fn set_ioapic_vector_enable(vector: usize, enable: bool) {
    let Some(gsi) = vector.checked_sub(IOAPIC_VECTOR_BASE).map(|gsi| gsi as u32) else {
        return;
    };

    let mut ioapics = IOAPICS.lock();
    let Some(ioapic) = ioapics.iter_mut().find(|ioapic| ioapic.contains(gsi)) else {
        return;
    };
    let Some(input) = ioapic.input_for(gsi) else {
        return;
    };

    unsafe {
        let mut entry = ioapic.ioapic.table_entry(input);
        entry.set_vector(vector as u8);
        entry.set_mode(IrqMode::Fixed);
        entry.set_flags(intx_flags(
            AcpiIrqTrigger::Level,
            AcpiIrqPolarity::ActiveLow,
        ));
        entry.set_dest(0);
        ioapic.ioapic.set_table_entry(input, entry);

        if enable {
            ioapic.ioapic.enable_irq(input);
        } else {
            ioapic.ioapic.disable_irq(input);
        }
    }
}

fn intx_flags(trigger: AcpiIrqTrigger, polarity: AcpiIrqPolarity) -> IrqFlags {
    let mut flags = IrqFlags::empty();
    if trigger == AcpiIrqTrigger::Level {
        flags |= IrqFlags::LEVEL_TRIGGERED;
    }
    if polarity == AcpiIrqPolarity::ActiveLow {
        flags |= IrqFlags::LOW_ACTIVE;
    }
    flags
}

fn lapic_eoi() {
    unsafe {
        lapic_write(LAPIC_REG_EOI, 0);
    }
}

fn raw_apic_id(id: usize) -> u32 {
    (id as u32) << 24
}

unsafe fn send_lapic_ipi(destination: u32, icr_low: u32) {
    unsafe {
        lapic_write(LAPIC_REG_ICR_HIGH, destination);
        lapic_write(LAPIC_REG_ICR_LOW, icr_low);
        while lapic_read(LAPIC_REG_ICR_LOW) & ICR_DELIVERY_PENDING != 0 {
            core::hint::spin_loop();
        }
    }
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
