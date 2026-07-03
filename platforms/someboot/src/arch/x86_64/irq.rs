use crate::irq::IrqId;

pub const LAPIC_TIMER_VECTOR: u8 = 0x20;
pub const LAPIC_TIMER_LOGICAL_IRQ: usize = LAPIC_TIMER_VECTOR as usize;
pub const LAPIC_SPURIOUS_VECTOR: u8 = 0xff;

pub fn systimer_irq() -> IrqId {
    IrqId::new(LAPIC_TIMER_LOGICAL_IRQ)
}
