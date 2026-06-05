// Ref: https://elixir.bootlin.com/linux/v6.16/source/drivers/irqchip/irq-loongson-eiointc.c

use loongArch64::iocsr::{iocsr_read_d, iocsr_write_d, iocsr_write_w};

const LOONGARCH_IOCSR_MISC_FUNC: usize = 0x420;
const IOCSR_MISC_FUNC_EXT_IOI_EN: u64 = 1 << 48;

const EIOINTC_REG_NODEMAP: usize = 0x14a0;
const EIOINTC_REG_IPMAP: usize = 0x14c0;
const EIOINTC_REG_ENABLE: usize = 0x1600;
const EIOINTC_REG_BOUNCE: usize = 0x1680;
const EIOINTC_REG_ISR: usize = 0x1800;
const EIOINTC_REG_ROUTE: usize = 0x1c00;

const VEC_REG_COUNT: usize = 4;
const VEC_COUNT_PER_REG: usize = 64;
const VEC_COUNT: usize = VEC_REG_COUNT * VEC_COUNT_PER_REG;

pub fn init() {
    // TODO: support smp
    let misc = iocsr_read_d(LOONGARCH_IOCSR_MISC_FUNC);
    iocsr_write_d(LOONGARCH_IOCSR_MISC_FUNC, misc | IOCSR_MISC_FUNC_EXT_IOI_EN);

    let index = 0;

    for i in 0..(VEC_COUNT / 32) {
        let data = ((1 << (i * 2 + 1)) << 16) | (1 << (i * 2));
        iocsr_write_w(EIOINTC_REG_NODEMAP + i * 4, data);
    }
    for i in 0..(VEC_COUNT / 32 / 4) {
        let bit = 1 << (1 + index);
        let data = bit | (bit << 8) | (bit << 16) | (bit << 24);
        iocsr_write_w(EIOINTC_REG_IPMAP + i * 4, data);
    }
    for i in 0..(VEC_COUNT / 4) {
        let bit = 1;
        let data = bit | (bit << 8) | (bit << 16) | (bit << 24);
        iocsr_write_w(EIOINTC_REG_ROUTE + i * 4, data);
    }
    for i in 0..(VEC_COUNT / 32) {
        iocsr_write_w(EIOINTC_REG_BOUNCE + i * 4, u32::MAX);
    }
}

fn split_bit(irq: usize) -> (usize, u64) {
    (irq / 64 * 8, 1 << (irq % 64))
}

pub fn enable_irq(irq: usize) {
    let (offset, bit) = split_bit(irq);
    for base in [EIOINTC_REG_ENABLE, EIOINTC_REG_BOUNCE] {
        let addr = base + offset;
        iocsr_write_d(addr, iocsr_read_d(addr) | bit);
    }
}
pub fn disable_irq(irq: usize) {
    let (offset, bit) = split_bit(irq);
    let addr = EIOINTC_REG_ENABLE + offset;
    iocsr_write_d(addr, iocsr_read_d(addr) & !bit);
}

pub fn claim_irq() -> Option<usize> {
    for i in 0..(VEC_COUNT / 64) {
        let flags = iocsr_read_d(EIOINTC_REG_ISR + i * 8);
        if flags != 0 {
            return Some(flags.trailing_zeros() as usize + 64 * i);
        }
    }
    None
}
pub fn complete_irq(irq: usize) {
    let (offset, bit) = split_bit(irq);
    iocsr_write_d(EIOINTC_REG_ISR + offset, bit);
}
