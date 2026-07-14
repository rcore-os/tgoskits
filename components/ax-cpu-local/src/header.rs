use core::{
    fmt,
    mem::{align_of, offset_of, size_of},
};

use crate::{CpuIndex, CpuLocalAnchor, PerCpuRelocation};

/// Default nonzero identity cookie for compatibility CPU-area layouts.
pub const CPU_AREA_DEFAULT_COOKIE: usize = 0x4158_4350;

/// Immutable identity of one installed CPU-local area.
///
/// The type occupies exactly one cache line on both 32-bit and 64-bit targets.
/// Its fields remain private so publication and validation cannot be bypassed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(64))]
pub struct CpuAreaHeader {
    self_base: usize,
    relocation: usize,
    cpu_index: u32,
    generation: u32,
    cookie: usize,
    reserved: [u8; 64 - 8 - 3 * size_of::<usize>()],
}

impl CpuAreaHeader {
    const TEMPLATE: Self = Self {
        self_base: 0,
        relocation: 0,
        cpu_index: CpuIndex::INVALID_RAW,
        generation: 0,
        cookie: 0,
        reserved: [0; 64 - 8 - 3 * size_of::<usize>()],
    };

    const fn for_area(
        cpu_index: CpuIndex,
        anchor: CpuLocalAnchor,
        generation: u32,
        cookie: usize,
    ) -> Self {
        Self {
            generation,
            cookie,
            self_base: anchor.area_base(),
            relocation: anchor.relocation().raw(),
            cpu_index: cpu_index.as_u32(),
            ..Self::TEMPLATE
        }
    }

    /// Returns the CPU index recorded in this header.
    pub const fn cpu_index(&self) -> Option<CpuIndex> {
        CpuIndex::from_u32(self.cpu_index)
    }

    /// Returns the runtime address recorded for this header.
    pub const fn self_base(&self) -> usize {
        self.self_base
    }

    /// Returns the per-CPU link-to-runtime relocation.
    pub const fn relocation(&self) -> PerCpuRelocation {
        PerCpuRelocation::from_raw(self.relocation)
    }

    /// Returns the layout generation that published this header.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Returns the layout identity cookie that published this header.
    pub const fn cookie(&self) -> usize {
        self.cookie
    }

    /// Returns whether this header still contains an unpublished template.
    ///
    /// Both the linker template and zero-initialized external storage satisfy
    /// this predicate. Once published, a header remains bound until shutdown.
    pub const fn is_unbound(&self) -> bool {
        self.self_base == 0 && self.generation == 0 && self.cookie == 0
    }

    /// Validates this immutable header against an expected installed area.
    ///
    /// This method deliberately borrows only the first cache line. Trap entry
    /// may concurrently use [`CpuEntryScratch`] in the second cache line.
    pub fn validate(
        &self,
        cpu_index: CpuIndex,
        anchor: CpuLocalAnchor,
        generation: u32,
        cookie: usize,
    ) -> Result<(), CpuAreaHeaderError> {
        if self.cpu_index != cpu_index.as_u32() {
            return Err(CpuAreaHeaderError::CpuIndex);
        }
        if self.self_base != anchor.area_base() || self.relocation != anchor.relocation().raw() {
            return Err(CpuAreaHeaderError::Anchor);
        }
        if self.generation != generation {
            return Err(CpuAreaHeaderError::Generation);
        }
        if self.cookie != cookie {
            return Err(CpuAreaHeaderError::Cookie);
        }
        Ok(())
    }
}

/// CPU-local state needed before architecture trap entry can use a Rust stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(64))]
pub struct CpuEntryScratch {
    /// Kernel continuation stack saved while user mode is active.
    pub kernel_stack_pointer: usize,
    /// Current user register frame used as the kernel trap stack.
    pub user_trap_frame: usize,
    /// Architecture entry scratch word zero.
    pub scratch0: usize,
    /// Architecture entry scratch word one.
    pub scratch1: usize,
    reserved: [u8; 64 - 4 * size_of::<usize>()],
}

impl CpuEntryScratch {
    const EMPTY: Self = Self {
        kernel_stack_pointer: 0,
        user_trap_frame: 0,
        scratch0: 0,
        scratch1: 0,
        reserved: [0; 64 - 4 * size_of::<usize>()],
    };
}

/// Fixed two-cache-line prefix at offset zero of every CPU-local area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(64))]
pub struct CpuAreaPrefix {
    header: CpuAreaHeader,
    entry: CpuEntryScratch,
}

impl CpuAreaPrefix {
    /// Prefix image copied from the link-time per-CPU template.
    pub const TEMPLATE: Self = Self {
        header: CpuAreaHeader::TEMPLATE,
        entry: CpuEntryScratch::EMPTY,
    };

    /// Creates a published prefix for one initialized area.
    pub const fn for_area(
        cpu_index: CpuIndex,
        anchor: CpuLocalAnchor,
        generation: u32,
        cookie: usize,
    ) -> Self {
        Self {
            header: CpuAreaHeader::for_area(cpu_index, anchor, generation, cookie),
            entry: CpuEntryScratch::EMPTY,
        }
    }

    /// Returns immutable CPU-area identity.
    pub const fn header(&self) -> &CpuAreaHeader {
        &self.header
    }

    /// Returns architecture entry scratch state.
    pub const fn entry_scratch(&self) -> &CpuEntryScratch {
        &self.entry
    }

    /// Returns mutable architecture entry scratch state.
    ///
    /// The caller must serialize trap entry against this mutation, normally by
    /// keeping local IRQs disabled while publishing a user entry context.
    pub fn entry_scratch_mut(&mut self) -> &mut CpuEntryScratch {
        &mut self.entry
    }

    /// Validates immutable identity against the expected installed area.
    pub fn validate(
        &self,
        cpu_index: CpuIndex,
        anchor: CpuLocalAnchor,
        generation: u32,
        cookie: usize,
    ) -> Result<(), CpuAreaHeaderError> {
        self.header.validate(cpu_index, anchor, generation, cookie)
    }
}

/// Size in bytes of the immutable [`CpuAreaHeader`].
pub const CPU_AREA_HEADER_SIZE: usize = size_of::<CpuAreaHeader>();
/// Byte offset of the immutable header in [`CpuAreaPrefix`].
pub const CPU_AREA_HEADER_OFFSET: usize = offset_of!(CpuAreaPrefix, header);
/// Byte offset of architecture entry scratch in [`CpuAreaPrefix`].
pub const CPU_AREA_ENTRY_OFFSET: usize = offset_of!(CpuAreaPrefix, entry);
/// Byte offset of the runtime self pointer from the prefix base.
pub const CPU_AREA_SELF_BASE_OFFSET: usize =
    CPU_AREA_HEADER_OFFSET + offset_of!(CpuAreaHeader, self_base);
/// Byte offset of the CPU-local relocation from the prefix base.
pub const CPU_AREA_RELOCATION_OFFSET: usize =
    CPU_AREA_HEADER_OFFSET + offset_of!(CpuAreaHeader, relocation);
/// Byte offset of the logical CPU index from the prefix base.
pub const CPU_AREA_CPU_INDEX_OFFSET: usize =
    CPU_AREA_HEADER_OFFSET + offset_of!(CpuAreaHeader, cpu_index);
/// Byte offset of the layout generation from the prefix base.
pub const CPU_AREA_GENERATION_OFFSET: usize =
    CPU_AREA_HEADER_OFFSET + offset_of!(CpuAreaHeader, generation);
/// Byte offset of the layout cookie from the prefix base.
pub const CPU_AREA_COOKIE_OFFSET: usize =
    CPU_AREA_HEADER_OFFSET + offset_of!(CpuAreaHeader, cookie);
/// Byte offset of the kernel continuation stack pointer.
pub const CPU_AREA_KERNEL_STACK_POINTER_OFFSET: usize =
    CPU_AREA_ENTRY_OFFSET + offset_of!(CpuEntryScratch, kernel_stack_pointer);
/// Byte offset of the current user trap frame.
pub const CPU_AREA_USER_TRAP_FRAME_OFFSET: usize =
    CPU_AREA_ENTRY_OFFSET + offset_of!(CpuEntryScratch, user_trap_frame);
/// Byte offset of architecture entry scratch word zero.
pub const CPU_AREA_ENTRY_SCRATCH0_OFFSET: usize =
    CPU_AREA_ENTRY_OFFSET + offset_of!(CpuEntryScratch, scratch0);
/// Byte offset of architecture entry scratch word one.
pub const CPU_AREA_ENTRY_SCRATCH1_OFFSET: usize =
    CPU_AREA_ENTRY_OFFSET + offset_of!(CpuEntryScratch, scratch1);

const _: () = {
    assert!(size_of::<CpuAreaHeader>() == 64);
    assert!(align_of::<CpuAreaHeader>() == 64);
    assert!(size_of::<CpuEntryScratch>() == 64);
    assert!(align_of::<CpuEntryScratch>() == 64);
    assert!(size_of::<CpuAreaPrefix>() == 128);
    assert!(align_of::<CpuAreaPrefix>() == 64);
    assert!(CPU_AREA_HEADER_OFFSET == 0);
    assert!(CPU_AREA_ENTRY_OFFSET == 64);
    assert!(CPU_AREA_SELF_BASE_OFFSET == 0);
    assert!(CPU_AREA_RELOCATION_OFFSET == size_of::<usize>());
    assert!(CPU_AREA_CPU_INDEX_OFFSET == 2 * size_of::<usize>());
    assert!(CPU_AREA_GENERATION_OFFSET == 2 * size_of::<usize>() + 4);
    assert!(CPU_AREA_COOKIE_OFFSET == 2 * size_of::<usize>() + 8);
};

/// Reason an installed CPU-area header failed verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuAreaHeaderError {
    /// Header belongs to another logical CPU.
    CpuIndex,
    /// Header runtime base or relocation differs from the installed anchor.
    Anchor,
    /// Header belongs to another initialization generation.
    Generation,
    /// Header belongs to another layout identity.
    Cookie,
}

impl fmt::Display for CpuAreaHeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::CpuIndex => "logical CPU mismatch",
            Self::Anchor => "runtime anchor mismatch",
            Self::Generation => "layout generation mismatch",
            Self::Cookie => "layout cookie mismatch",
        };
        formatter.write_str(reason)
    }
}

impl core::error::Error for CpuAreaHeaderError {}

#[doc(hidden)]
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".percpu.000.header")]
pub static mut __AX_CPU_AREA_PREFIX: CpuAreaPrefix = CpuAreaPrefix::TEMPLATE;

/// One-byte sentinel that the linker must retain after every ordinary
/// CPU-local template input section.
///
/// Its address plus its size is the exclusive template end. Keeping this
/// boundary in the architecture leaf lets higher layers validate layouts
/// without importing linker symbols or guessing whether a target triple is a
/// hosted process or a kernel image.
#[doc(hidden)]
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".percpu_end")]
pub static __AX_CPU_AREA_TEMPLATE_END: u8 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_abi_keeps_header_and_entry_on_separate_cache_lines() {
        assert_eq!(size_of::<CpuAreaHeader>(), 64);
        assert_eq!(size_of::<CpuEntryScratch>(), 64);
        assert_eq!(size_of::<CpuAreaPrefix>(), 128);
        assert_eq!(CPU_AREA_HEADER_OFFSET, 0);
        assert_eq!(CPU_AREA_ENTRY_OFFSET, 64);
        assert_eq!(CPU_AREA_SELF_BASE_OFFSET, 0);
        assert_eq!(CPU_AREA_RELOCATION_OFFSET, size_of::<usize>());
        assert_eq!(CPU_AREA_CPU_INDEX_OFFSET, 2 * size_of::<usize>());
        assert_eq!(CPU_AREA_GENERATION_OFFSET, 2 * size_of::<usize>() + 4);
        assert_eq!(CPU_AREA_COOKIE_OFFSET, 2 * size_of::<usize>() + 8);
        assert_eq!(CPU_AREA_KERNEL_STACK_POINTER_OFFSET, 64);
        assert_eq!(CPU_AREA_USER_TRAP_FRAME_OFFSET, 64 + size_of::<usize>());
    }

    #[test]
    fn validation_rejects_stale_generation_and_foreign_cookie() {
        let cpu = CpuIndex::try_from(3).unwrap();
        let anchor = CpuLocalAnchor::new(0x8000, PerCpuRelocation::from_raw(0x7000));
        let prefix = CpuAreaPrefix::for_area(cpu, anchor, 11, 0x55aa);

        assert_eq!(prefix.validate(cpu, anchor, 11, 0x55aa), Ok(()));
        assert_eq!(
            prefix.validate(cpu, anchor, 12, 0x55aa),
            Err(CpuAreaHeaderError::Generation)
        );
        assert_eq!(
            prefix.validate(cpu, anchor, 11, 0xaa55),
            Err(CpuAreaHeaderError::Cookie)
        );
    }
}
