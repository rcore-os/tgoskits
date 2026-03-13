//! RISC-V Platform-Level Interrupt Controller
//! <https://github.com/riscv/riscv-plic-spec/blob/master/riscv-plic.adoc>

#![no_std]

use core::num::NonZeroU32;
use core::ptr::NonNull;

use tock_registers::{
    fields::Field,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_structs,
    registers::{ReadOnly, ReadWrite},
};

/// See §1.
const SOURCE_NUM: usize = 1024;
/// See §1.
const CONTEXT_NUM: usize = 15872;

const U32_BITS: usize = u32::BITS as usize;

register_structs! {
    ContextLocal {
        /// Priority Threshold
        /// - The base address of Priority Thresholds register block is located at 4K alignment starts from offset 0x200000.
        (0x0000 => priority_threshold: ReadWrite<u32>),
        /// Interrupt Claim/complete Process
        /// - The Interrupt Claim Process register is context based and is located at (4K alignment + 4) starts from offset 0x200000.
        (0x0004 => interrupt_claim_complete: ReadWrite<u32>),
        (0x0008 => _reserved_0),
        (0x1000 => @END),
    }
}

register_structs! {
    /// PLIC registers
    pub PLICRegs {
        /// Interrupt Source Priority #0 to #1023
        (0x000000 => interrupt_priority: [ReadWrite<u32>; SOURCE_NUM]),
        /// Interrupt Pending Bit of Interrupt Source #0 to #N
        /// 0x001000: Interrupt Source #0 to #31 Pending Bits
        /// ...
        /// 0x00107C: Interrupt Source #992 to #1023 Pending Bits
        (0x001000 => interrupt_pending: [ReadOnly<u32>; SOURCE_NUM / U32_BITS]),
        (0x001080 => _reserved_0),
        /// Interrupt Enable Bit of Interrupt Source #0 to #1023 for 15872 contexts
        (0x002000 => interrupt_enable: [[ReadWrite<u32>; SOURCE_NUM / U32_BITS]; CONTEXT_NUM]),
        (0x1F2000 => _reserved_1),
        /// 4096 * 15872 = 65011712(0x3e000 00) bytes
        /// Priority Threshold for 15872 contexts
        /// - The base address of Priority Thresholds register block is located at 4K alignment starts from offset 0x200000.
        /// Interrupt Claim Process for 15872 contexts
        /// - The Interrupt Claim Process register is context based and is located at (4K alignment + 4) starts from offset 0x200000.
        /// - The Interrupt Completion registers are context based and located at the same address with Interrupt Claim Process register, which is at (4K alignment + 4) starts from offset 0x200000.
        (0x200000 => contexts: [ContextLocal; CONTEXT_NUM]),
        (0x4000000 => @END),
    }
}

/// Platform-Level Interrupt Controller.
pub struct Plic {
    base: NonNull<PLICRegs>,
}

unsafe impl Send for Plic {}
unsafe impl Sync for Plic {}

impl Plic {
    /// Create a new instance of the PLIC from the base address.
    ///
    /// # Safety
    ///
    /// `base` must be a unique valid pointer to PLIC memory-mapped registers.
    #[inline]
    pub const unsafe fn new(base: NonNull<PLICRegs>) -> Self {
        Self { base }
    }

    /// Initialize the PLIC by context, setting the priority threshold to 0.
    pub fn init_by_context(&mut self, ctx: usize) {
        self.regs().contexts[ctx].priority_threshold.set(0);
    }

    const fn regs(&self) -> &PLICRegs {
        unsafe { self.base.as_ref() }
    }

    /// Sets priority for interrupt `source` to `value`.
    ///
    /// Write `0` to priority `value` effectively disables this interrupt `source`, for the priority
    /// value 0 is reserved for "never interrupt" by the PLIC specification.
    ///
    /// The lowest active priority is priority `1`. The maximum priority depends on PLIC implementation
    /// and can be detected with [`Plic::probe_priority_bits`].
    ///
    /// See §4.
    #[inline]
    pub fn set_priority(&mut self, source: NonZeroU32, value: u32) {
        self.regs().interrupt_priority[source.get() as usize].set(value);
    }

    /// Gets priority for interrupt `source`.
    ///
    /// See §4.
    #[inline]
    pub fn get_priority(&self, source: NonZeroU32) -> u32 {
        self.regs().interrupt_priority[source.get() as usize].get()
    }

    /// Probe maximum level of priority for interrupt `source`.
    ///
    /// See §4.
    #[inline]
    pub fn probe_priority_bits(&mut self, source: NonZeroU32) -> u32 {
        self.regs().interrupt_priority[source.get() as usize].set(!0);
        self.regs().interrupt_priority[source.get() as usize].get()
    }

    /// Check if interrupt `source` is pending.
    ///
    /// See §5.
    #[inline]
    pub fn is_pending(&self, source: NonZeroU32) -> bool {
        let (group, field) = parse_group_and_field(source.get() as usize);
        self.regs().interrupt_pending[group].read(field) != 0
    }

    /// Enable interrupt `source` in `context`.
    ///
    /// See §6.
    #[inline]
    pub fn enable(&mut self, source: NonZeroU32, ctx: usize) {
        let (group, field) = parse_group_and_field(source.get() as usize);

        self.regs().interrupt_enable[ctx][group].modify(field.val(1));
    }

    /// Disable interrupt `source` in `context`.
    ///
    /// See §6.
    #[inline]
    pub fn disable(&mut self, source: NonZeroU32, ctx: usize) {
        let (group, field) = parse_group_and_field(source.get() as usize);

        self.regs().interrupt_enable[ctx][group].modify(field.val(0));
    }

    /// Check if interrupt `source` is enabled in `context`.
    ///
    /// See §6.
    #[inline]
    pub fn is_enabled(&self, source: NonZeroU32, ctx: usize) -> bool {
        let (group, field) = parse_group_and_field(source.get() as usize);

        self.regs().interrupt_enable[ctx][group].read(field) != 0
    }

    /// Get interrupt threshold in `context`.
    ///
    /// See §7.
    #[inline]
    pub fn get_threshold(&self, ctx: usize) -> u32 {
        self.regs().contexts[ctx].priority_threshold.get()
    }

    /// Set interrupt threshold for `context` to `value`.
    ///
    /// See §7.
    #[inline]
    pub fn set_threshold(&mut self, ctx: usize, value: u32) {
        self.regs().contexts[ctx].priority_threshold.set(value);
    }

    /// Probe maximum supported threshold value the `context` supports.
    ///
    /// See §7.
    #[inline]
    pub fn probe_threshold_bits(&mut self, ctx: usize) -> u32 {
        self.regs().contexts[ctx].priority_threshold.set(!0);
        self.regs().contexts[ctx].priority_threshold.get()
    }

    /// Claim an interrupt in `context`, returning its source.
    ///
    /// It is always legal for a hart to perform a claim even if `EIP` is not set.
    /// A hart could set threshold to maximum to disable interrupt notification, but it does not mean
    /// interrupt source has stopped to send interrupt signals. In this case, hart would instead
    /// poll for active interrupt by periodically calling the `claim` function.
    ///
    /// See §8.
    #[inline]
    pub fn claim(&mut self, ctx: usize) -> Option<NonZeroU32> {
        NonZeroU32::new(self.regs().contexts[ctx].interrupt_claim_complete.get())
    }

    /// Mark that interrupt identified by `source` is completed in `context`.
    ///
    /// See §9.
    #[inline]
    pub fn complete(&mut self, ctx: usize, source: NonZeroU32) {
        self.regs().contexts[ctx]
            .interrupt_claim_complete
            .set(source.get());
    }
}

fn parse_group_and_field(source: usize) -> (usize, Field<u32, ()>) {
    let group = source / U32_BITS;
    let index = source % U32_BITS;
    let field = Field::<u32, ()>::new(0b1, index);
    (group, field)
}
