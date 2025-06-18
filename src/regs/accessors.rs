use super::GeneralRegisters;

impl GeneralRegisters {
    /// Returns the value of the general-purpose register corresponding to the given index.
    ///
    /// The mapping of indices to registers is as follows:
    /// - 0: `rax`
    /// - 1: `rcx`
    /// - 2: `rdx`
    /// - 3: `rbx`
    /// - 5: `rbp`
    /// - 6: `rsi`
    /// - 7: `rdi`
    /// - 8: `r8`
    /// - 9: `r9`
    /// - 10: `r10`
    /// - 11: `r11`
    /// - 12: `r12`
    /// - 13: `r13`
    /// - 14: `r14`
    /// - 15: `r15`
    ///
    /// # Panics
    ///
    /// This function will panic if the provided index is out of the range [0, 15] or if the index
    /// corresponds to an unused register (`rsp` at index 4).
    ///
    /// # Arguments
    ///
    /// * `index` - A `u8` value representing the index of the register.
    ///
    /// # Returns
    ///
    /// * `u64` - The value of the corresponding general-purpose register.
    pub fn get_reg_of_index(&self, index: u8) -> u64 {
        match index {
            0 => self.rax,
            1 => self.rcx,
            2 => self.rdx,
            3 => self.rbx,
            // 4 => self._unused_rsp,
            5 => self.rbp,
            6 => self.rsi,
            7 => self.rdi,
            8 => self.r8,
            9 => self.r9,
            10 => self.r10,
            11 => self.r11,
            12 => self.r12,
            13 => self.r13,
            14 => self.r14,
            15 => self.r15,
            _ => {
                panic!("Illegal index of GeneralRegisters {}", index);
            }
        }
    }

    /// Sets the value of the general-purpose register corresponding to the given index.
    ///
    /// The mapping of indices to registers is as follows:
    /// - 0: `rax`
    /// - 1: `rcx`
    /// - 2: `rdx`
    /// - 3: `rbx`
    /// - 5: `rbp`
    /// - 6: `rsi`
    /// - 7: `rdi`
    /// - 8: `r8`
    /// - 9: `r9`
    /// - 10: `r10`
    /// - 11: `r11`
    /// - 12: `r12`
    /// - 13: `r13`
    /// - 14: `r14`
    /// - 15: `r15`
    ///
    /// # Panics
    ///
    /// This function will panic if the provided index is out of the range [0, 15] or if the index
    /// corresponds to an unused register (`rsp` at index 4).
    ///
    /// # Arguments
    ///
    /// * `index` - A `u8` value representing the index of the register.
    ///
    /// # Returns
    ///
    /// * `u64` - The value of the corresponding general-purpose register.
    pub fn set_reg_of_index(&mut self, index: u8, value: u64) {
        match index {
            0 => self.rax = value,
            1 => self.rcx = value,
            2 => self.rdx = value,
            3 => self.rbx = value,
            // 4 => self._unused_rsp,
            5 => self.rbp = value,
            6 => self.rsi = value,
            7 => self.rdi = value,
            8 => self.r8 = value,
            9 => self.r9 = value,
            10 => self.r10 = value,
            11 => self.r11 = value,
            12 => self.r12 = value,
            13 => self.r13 = value,
            14 => self.r14 = value,
            15 => self.r15 = value,
            _ => {
                panic!("Illegal index of GeneralRegisters {}", index);
            }
        }
    }

    /// Returns the value of the `edx:eax` register pair.
    pub fn get_edx_eax(&self) -> u64 {
        (self.edx() as u64) << 32 | self.eax() as u64
    }
}

macro_rules! define_reg_getter_setters {
    ([$(($name:ident, $from:ident)),+ $(,)?], $type:ty, $bits:literal, clear_other_bits = $clear_other_bits:expr $(,)?) => {
        $(
            define_reg_getter_setters!(__impl_getter $name, $from, $type, 0..$bits);
            define_reg_getter_setters!(__impl_setter $name, $from, $type, 0..$bits, $clear_other_bits);
        )+
    };
    ([$(($name:ident, $from:ident)),+ $(,)?], $type:ty, $bits_start:literal..$bits_end:literal, clear_other_bits = $clear_other_bits:expr $(,)?) => {
        $(
            define_reg_getter_setters!(__impl_getter $name, $from, $type, $bits_start..$bits_end);
            define_reg_getter_setters!(__impl_setter $name, $from, $type, $bits_start..$bits_end, $clear_other_bits);
        )+
    };
    (__impl_getter $name:ident, $from:ident, $type:ty, $bits_start:literal..$bits_end:literal) => {
        paste::paste! {
            #[inline]
            #[doc = "Returns the value of the \"" $name "\" register, which is the bits " $bits_start "(inc.) to " $bits_end "(exc.) of the \"" $from "\" register."]
            pub fn $name(&self) -> $type {
                const MASK: u64 = (1u64 << ($bits_end - $bits_start)) - 1; // as `bits` will never be greater than 64, this is safe

                let mut value = self.$from;
                value >>= $bits_start;
                value &= MASK;
                value as $type
            }
        }
    };
    (__impl_setter $name:ident, $from:ident, $type:ty, $bits_start:literal..$bits_end:literal, $clear_other_bits:expr) => {
        paste::paste! {
            #[inline]
            #[doc = "Sets the value of the \"" $name "\" register, which is the bits " $bits_start "(inc.) to " $bits_end "(exc.) of the \"" $from "\" register."]
            #[doc = ""]
            #[doc = "Whether the other bits of the \"" $from "\" register should be cleared follows the rules of the x86-64 architecture."]
            pub fn [< set_ $name >](&mut self, value: $type) {
                if $clear_other_bits {
                    self.$from = (value as u64) << $bits_start;
                } else {
                    const MASK: u64 = ((1u64 << ($bits_end - $bits_start)) - 1) << $bits_start;
                    self.$from &= !MASK;
                    self.$from |= (value as u64) << $bits_start;
                }
            }
        }
    }
}

impl GeneralRegisters {
    define_reg_getter_setters!(
        [
            (eax, rax),
            (ecx, rcx),
            (edx, rdx),
            (ebx, rbx),
            (ebp, rbp),
            (esi, rsi),
            (edi, rdi),
            (r8d, r8),
            (r9d, r9),
            (r10d, r10),
            (r11d, r11),
            (r12d, r12),
            (r13d, r13),
            (r14d, r14),
            (r15d, r15),
        ],
        u32,
        32,
        clear_other_bits = true,
    );

    define_reg_getter_setters!(
        [
            (ax, rax),
            (cx, rcx),
            (dx, rdx),
            (bx, rbx),
            (bp, rbp),
            (si, rsi),
            (di, rdi),
            (r8w, r8),
            (r9w, r9),
            (r10w, r10),
            (r11w, r11),
            (r12w, r12),
            (r13w, r13),
            (r14w, r14),
            (r15w, r15),
        ],
        u16,
        16,
        clear_other_bits = false,
    );

    define_reg_getter_setters!(
        [
            (al, rax),
            (cl, rcx),
            (dl, rdx),
            (bl, rbx),
            (bpl, rbp),
            (sil, rsi),
            (dil, rdi),
            (r8b, r8),
            (r9b, r9),
            (r10b, r10),
            (r11b, r11),
            (r12b, r12),
            (r13b, r13),
            (r14b, r14),
            (r15b, r15),
        ],
        u8,
        8,
        clear_other_bits = false,
    );

    define_reg_getter_setters!(
        [(ah, rax), (ch, rcx), (dh, rdx), (bh, rbx),],
        u8,
        8..16,
        clear_other_bits = false,
    );
}

#[cfg(test)]
mod test {
    use super::*;

    macro_rules! test_rw_on_reg {
        ([$(($pos:literal, $reg:ident, $reg32:ident, $reg16:ident, $reg8:ident $(, $reg8h:ident)? $(,)?)),+ $(,)?]) => {
            paste::paste! {
                $(
                    #[test]
                    fn [< test_read_write_on_reg_ $reg >]() {
                        let mut regs = GeneralRegisters::default();
                        regs.$reg = 0xfedcba9876543210;
                        assert_eq!(regs.get_reg_of_index($pos), 0xfedcba9876543210);

                        regs.set_reg_of_index($pos, 0x123456789abcdef0);
                        assert_eq!(regs.$reg, 0x123456789abcdef0);

                        $(
                            regs.[< set_ $reg8h >](0x12);
                            assert_eq!(regs.$reg, 0x123456789abc12f0);
                            regs.[< set_ $reg8h >](0xde);
                        )?

                        regs.[< set_ $reg8 >](0x34);
                        assert_eq!(regs.$reg, 0x123456789abcde34);

                        regs.[< set_ $reg16 >](0x5678);
                        assert_eq!(regs.$reg, 0x123456789abc5678);

                        regs.[< set_ $reg32 >](0x9abcdef0);
                        assert_eq!(regs.$reg, 0x9abcdef0);
                    }
                )+
            }
        };
    }

    test_rw_on_reg!([
        (0, rax, eax, ax, al, ah),
        (1, rcx, ecx, cx, cl, ch),
        (2, rdx, edx, dx, dl, dh),
        (3, rbx, ebx, bx, bl, bh),
        (5, rbp, ebp, bp, bpl),
        (6, rsi, esi, si, sil),
        (7, rdi, edi, di, dil),
        (8, r8, r8d, r8w, r8b),
        (9, r9, r9d, r9w, r9b),
        (10, r10, r10d, r10w, r10b),
        (11, r11, r11d, r11w, r11b),
        (12, r12, r12d, r12w, r12b),
        (13, r13, r13d, r13w, r13b),
        (14, r14, r14d, r14w, r14b),
        (15, r15, r15d, r15w, r15b),
    ]);
}
