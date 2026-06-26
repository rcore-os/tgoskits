// Ref: https://elixir.bootlin.com/linux/v6.16/source/drivers/irqchip/irq-loongson-pch-pic.c

use crate::config::{devices::PCH_PIC_PADDR, plat::PHYS_VIRT_OFFSET};

const PIC_COUNT_PER_REG: usize = 32;
const PIC_REG_COUNT: usize = 2;
const PIC_IRQ_COUNT: usize = PIC_COUNT_PER_REG * PIC_REG_COUNT;

const PCH_PIC_MASK: usize = 0x20;
const PCH_PIC_EDGE: usize = 0x60;
const PCH_PIC_POL: usize = 0x3e0;
const PCH_INT_HTVEC: usize = 0x200;

const MMIO_BASE: usize = PHYS_VIRT_OFFSET + PCH_PIC_PADDR;

fn read_w(addr: usize) -> u32 {
    unsafe { ((MMIO_BASE + addr) as *mut u32).read_volatile() }
}

fn write_w(addr: usize, val: u32) {
    unsafe {
        ((MMIO_BASE + addr) as *mut u32).write_volatile(val);
    }
}

pub fn init() {
    // High level triggered
    for i in 0..PIC_REG_COUNT {
        let offset = i * 4;
        write_w(PCH_PIC_MASK + offset, u32::MAX);
        write_w(PCH_PIC_EDGE + offset, 0);
        write_w(PCH_PIC_POL + offset, 0);
    }
}

fn split_bit(irq: usize) -> (usize, u32) {
    (irq / PIC_COUNT_PER_REG * 4, 1 << (irq % PIC_COUNT_PER_REG))
}

const fn valid_irq(irq: usize) -> bool {
    irq < PIC_IRQ_COUNT
}

pub fn vector_for_input(input: usize) -> Option<usize> {
    valid_irq(input).then_some(input)
}

pub fn input_for_vector(vector: usize) -> Option<usize> {
    valid_irq(vector).then_some(vector)
}

pub fn enable_irq(irq: usize) {
    if !valid_irq(irq) {
        return;
    }
    let (offset, bit) = split_bit(irq);

    let addr = PCH_PIC_MASK + offset;
    write_w(addr, read_w(addr) & !bit);

    let addr = PCH_INT_HTVEC + irq;
    unsafe {
        ((MMIO_BASE + addr) as *mut u8).write_volatile(irq as _);
    }
}

pub fn disable_irq(irq: usize) {
    if !valid_irq(irq) {
        return;
    }
    let (offset, bit) = split_bit(irq);
    let addr = PCH_PIC_MASK + offset;
    write_w(addr, read_w(addr) | bit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pch_pic_accepts_only_two_mask_registers_worth_of_irqs() {
        assert!(valid_irq(0));
        assert!(valid_irq(63));
        assert!(!valid_irq(64));
    }

    #[test]
    fn pch_pic_irq_bits_split_on_32_bit_mask_registers() {
        assert_eq!(split_bit(0), (0, 1));
        assert_eq!(split_bit(31), (0, 1u32 << 31));
        assert_eq!(split_bit(32), (4, 1));
        assert_eq!(split_bit(63), (4, 1u32 << 31));
    }
}
