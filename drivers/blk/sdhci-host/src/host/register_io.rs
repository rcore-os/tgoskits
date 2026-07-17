//! Register-width adaptation for controllers that only accept aligned words.

use core::cell::Cell;

use crate::regs::{
    REG_BLOCK_COUNT, REG_BLOCK_SIZE, REG_COMMAND, REG_NORMAL_INT_STATUS, REG_TRANSFER_MODE,
};

pub(super) trait WordIo {
    fn read_u32(&self, aligned_offset: usize) -> u32;
    fn write_u32(&self, aligned_offset: usize, value: u32);
}

pub(super) struct MmioWords {
    base_addr: usize,
}

impl MmioWords {
    pub(super) const fn new(base_addr: usize) -> Self {
        Self { base_addr }
    }
}

impl WordIo for MmioWords {
    fn read_u32(&self, aligned_offset: usize) -> u32 {
        // SAFETY: `Sdhci` construction requires a valid, exclusively owned
        // register mapping. Callers pass only aligned SDHCI offsets.
        unsafe { core::ptr::read_volatile((self.base_addr + aligned_offset) as *const u32) }
    }

    fn write_u32(&self, aligned_offset: usize, value: u32) {
        // SAFETY: The same mapping/alignment contract as `read_u32` applies.
        unsafe {
            core::ptr::write_volatile((self.base_addr + aligned_offset) as *mut u32, value);
        }
    }
}

pub(super) struct Aligned32RegisterFile {
    block_shadow: Cell<u32>,
    block_shadow_valid: Cell<bool>,
    command_shadow: Cell<u32>,
    command_shadow_valid: Cell<bool>,
}

impl Aligned32RegisterFile {
    pub(super) const fn new() -> Self {
        Self {
            block_shadow: Cell::new(0),
            block_shadow_valid: Cell::new(false),
            command_shadow: Cell::new(0),
            command_shadow_valid: Cell::new(false),
        }
    }

    pub(super) fn read_u8(&self, io: &impl WordIo, offset: usize) -> u8 {
        let value = io.read_u32(aligned_offset(offset));
        ((value >> field_shift(offset)) & u32::from(u8::MAX)) as u8
    }

    pub(super) fn read_u16(&self, io: &impl WordIo, offset: usize) -> u16 {
        let value = if matches!(offset, REG_BLOCK_SIZE | REG_BLOCK_COUNT)
            && self.block_shadow_valid.get()
        {
            self.block_shadow.get()
        } else if offset == REG_TRANSFER_MODE && self.command_shadow_valid.get() {
            self.command_shadow.get()
        } else {
            io.read_u32(aligned_offset(offset))
        };
        ((value >> field_shift(offset)) & u32::from(u16::MAX)) as u16
    }

    pub(super) fn write_u8(&self, io: &impl WordIo, offset: usize, value: u8) {
        let aligned = aligned_offset(offset);
        let shift = field_shift(offset);
        let current = io.read_u32(aligned);
        let next = replace_field(current, u32::from(u8::MAX), shift, u32::from(value));
        io.write_u32(aligned, next);
    }

    pub(super) fn write_u16(&self, io: &impl WordIo, offset: usize, value: u16) {
        let aligned = aligned_offset(offset);
        let shift = field_shift(offset);
        let current = self.word_for_write(io, offset);
        let next = replace_field(current, u32::from(u16::MAX), shift, u32::from(value));

        match offset {
            REG_BLOCK_SIZE | REG_BLOCK_COUNT => {
                self.block_shadow.set(next);
                self.block_shadow_valid.set(true);
            }
            REG_TRANSFER_MODE => {
                self.command_shadow.set(next);
                self.command_shadow_valid.set(true);
            }
            REG_COMMAND => self.commit_command(io, next),
            _ => io.write_u32(aligned, next),
        }
    }

    pub(super) fn ack_irq_status(&self, io: &impl WordIo, normal: u16, error: u16) {
        let value = u32::from(normal) | (u32::from(error) << 16);
        if value != 0 {
            io.write_u32(REG_NORMAL_INT_STATUS, value);
        }
    }

    pub(super) fn flush_block_shadow(&self, io: &impl WordIo) -> bool {
        if !self.block_shadow_valid.replace(false) {
            return false;
        }
        io.write_u32(REG_BLOCK_SIZE, self.block_shadow.get());
        true
    }

    fn word_for_write(&self, io: &impl WordIo, offset: usize) -> u32 {
        if matches!(offset, REG_BLOCK_SIZE | REG_BLOCK_COUNT) && self.block_shadow_valid.get() {
            self.block_shadow.get()
        } else if matches!(offset, REG_TRANSFER_MODE | REG_COMMAND)
            && self.command_shadow_valid.get()
        {
            self.command_shadow.get()
        } else {
            io.read_u32(aligned_offset(offset))
        }
    }

    fn commit_command(&self, io: &impl WordIo, command_word: u32) {
        self.flush_block_shadow(io);
        self.command_shadow_valid.set(false);
        io.write_u32(REG_TRANSFER_MODE, command_word);
    }
}

const fn aligned_offset(offset: usize) -> usize {
    offset & !3
}

const fn field_shift(offset: usize) -> u32 {
    ((offset & 3) * 8) as u32
}

const fn replace_field(current: u32, mask: u32, shift: u32, value: u32) -> u32 {
    (current & !(mask << shift)) | ((value & mask) << shift)
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use super::{Aligned32RegisterFile, WordIo};
    use crate::regs::{
        REG_BLOCK_COUNT, REG_BLOCK_SIZE, REG_COMMAND, REG_ERROR_INT_STATUS, REG_NORMAL_INT_STATUS,
        REG_TRANSFER_MODE,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Access {
        Read32(usize),
        Write32(usize, u32),
    }

    struct TraceWords {
        words: RefCell<[u32; 64]>,
        accesses: RefCell<alloc::vec::Vec<Access>>,
    }

    impl TraceWords {
        fn new() -> Self {
            Self {
                words: RefCell::new([0; 64]),
                accesses: RefCell::new(alloc::vec::Vec::new()),
            }
        }

        fn writes(&self) -> alloc::vec::Vec<Access> {
            self.accesses
                .borrow()
                .iter()
                .copied()
                .filter(|access| matches!(access, Access::Write32(..)))
                .collect()
        }
    }

    impl WordIo for TraceWords {
        fn read_u32(&self, aligned_offset: usize) -> u32 {
            self.accesses
                .borrow_mut()
                .push(Access::Read32(aligned_offset));
            self.words.borrow()[aligned_offset / 4]
        }

        fn write_u32(&self, aligned_offset: usize, value: u32) {
            self.accesses
                .borrow_mut()
                .push(Access::Write32(aligned_offset, value));
            self.words.borrow_mut()[aligned_offset / 4] = value;
        }
    }

    #[test]
    fn broadcom_shadow_commits_each_register_pair_with_one_word_write() {
        let io = TraceWords::new();
        let registers = Aligned32RegisterFile::new();

        registers.write_u16(&io, REG_BLOCK_SIZE, 512);
        registers.write_u16(&io, REG_BLOCK_COUNT, 3);
        registers.write_u16(&io, REG_TRANSFER_MODE, 0x23);
        assert!(io.writes().is_empty());

        registers.write_u16(&io, REG_COMMAND, 0x113a);

        assert_eq!(
            io.writes(),
            alloc::vec![
                Access::Write32(REG_BLOCK_SIZE, 3 << 16 | 512),
                Access::Write32(REG_TRANSFER_MODE, 0x113a << 16 | 0x23),
            ]
        );
    }

    #[test]
    fn broadcom_irq_ack_is_one_word_w1c_without_read_modify_write() {
        let io = TraceWords::new();
        let registers = Aligned32RegisterFile::new();

        registers.ack_irq_status(&io, 0x8001, 0x0010);

        assert_eq!(
            io.accesses.into_inner(),
            alloc::vec![Access::Write32(REG_NORMAL_INT_STATUS, 0x0010_8001)]
        );
        assert_eq!(REG_ERROR_INT_STATUS, REG_NORMAL_INT_STATUS + 2);
    }

    #[test]
    fn broadcom_narrow_fields_are_derived_only_from_aligned_word_reads() {
        let io = TraceWords::new();
        io.words.borrow_mut()[0x28 / 4] = 0xa1b2_c3d4;
        let registers = Aligned32RegisterFile::new();

        assert_eq!(registers.read_u8(&io, 0x29), 0xc3);
        assert_eq!(registers.read_u16(&io, 0x2a), 0xa1b2);
        assert_eq!(
            io.accesses.into_inner(),
            alloc::vec![Access::Read32(0x28), Access::Read32(0x28)]
        );
    }
}
