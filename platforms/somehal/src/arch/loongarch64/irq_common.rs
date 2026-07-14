pub const EIOINTC_VECTOR_COUNT: usize = 256;
pub const LIOINTC_VECTOR_COUNT: usize = 32;
pub const PCH_PIC_VECTOR_COUNT: usize = 64;

pub fn fdt_first_cell_vector(irq_prop: &[u32]) -> Option<usize> {
    irq_prop.first().copied().map(|vector| vector as usize)
}

pub fn eiointc_reg_bit(irq: usize) -> (usize, u64) {
    (irq / 64 * 8, 1u64 << (irq % 64))
}

pub fn pch_pic_reg_bit(irq: usize) -> (usize, u32) {
    (irq / 32 * 4, 1u32 << (irq % 32))
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    #[test]
    fn fdt_first_cell_vector_uses_first_specifier_cell() {
        assert_eq!(fdt_first_cell_vector(&[0x2a]), Some(0x2a));
        assert_eq!(fdt_first_cell_vector(&[0x11, 0x1]), Some(0x11));
    }

    #[test]
    fn fdt_first_cell_vector_rejects_empty_specifier() {
        assert_eq!(fdt_first_cell_vector(&[]), None);
    }

    #[test]
    fn eiointc_reg_bit_splits_64_bit_registers() {
        assert_eq!(eiointc_reg_bit(0), (0, 1));
        assert_eq!(eiointc_reg_bit(63), (0, 1u64 << 63));
        assert_eq!(eiointc_reg_bit(64), (8, 1));
        assert_eq!(eiointc_reg_bit(255), (24, 1u64 << 63));
    }

    #[test]
    fn pch_pic_reg_bit_splits_32_bit_registers() {
        assert_eq!(pch_pic_reg_bit(0), (0, 1));
        assert_eq!(pch_pic_reg_bit(31), (0, 1u32 << 31));
        assert_eq!(pch_pic_reg_bit(32), (4, 1));
        assert_eq!(pch_pic_reg_bit(63), (4, 1u32 << 31));
    }
}
