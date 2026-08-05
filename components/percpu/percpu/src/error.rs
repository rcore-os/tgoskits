use cpu_local::{CpuAreaRef, CpuIndex, CpuLocalError};

/// Failure to initialize, locate, or access a runtime per-CPU area.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PerCpuError {
    /// One area cannot hold the fixed CPU-local prefix.
    #[error("per-CPU template size {actual:#x} is smaller than {minimum:#x}")]
    AreaTooSmall {
        /// Actual linked template size.
        actual: usize,
        /// Minimum supported prefix size.
        minimum: usize,
    },
    /// Adjacent runtime areas overlap.
    #[error("per-CPU stride {stride:#x} is smaller than area size {area_size:#x}")]
    StrideTooSmall {
        /// Supplied stride.
        stride: usize,
        /// Linked template size.
        area_size: usize,
    },
    /// Runtime base is not aligned for all generated symbols.
    #[error("per-CPU runtime base {base:#x} is not aligned to {alignment:#x}")]
    MisalignedRuntimeBase {
        /// Supplied runtime base.
        base: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// Area stride does not preserve required alignment.
    #[error("per-CPU stride {stride:#x} is not aligned to {alignment:#x}")]
    MisalignedStride {
        /// Supplied stride.
        stride: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// The linked template base does not preserve every symbol's alignment.
    #[error("per-CPU template base {base:#x} is not aligned to {alignment:#x}")]
    MisalignedTemplateBase {
        /// Loaded template base.
        base: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// Linker-provided alignment boundaries are inconsistent.
    #[error("per-CPU alignment metadata range {start:#x}..{end:#x} is malformed")]
    MalformedAlignmentMetadata {
        /// First descriptor address.
        start: usize,
        /// One-past-the-end descriptor address.
        end: usize,
    },
    /// A generated alignment is not a nonzero power of two.
    #[error("per-CPU symbol alignment descriptor {0:#x} is invalid")]
    InvalidSymbolAlignment(usize),
    /// The linker layout and generated descriptor table disagree.
    #[error(
        "per-CPU alignment descriptors require {descriptors:#x}, but linker reports {linker:#x}"
    )]
    AlignmentMetadataMismatch {
        /// Maximum generated alignment.
        descriptors: usize,
        /// Alignment encoded by the linker.
        linker: usize,
    },
    /// Address calculation overflowed.
    #[error("per-CPU layout address calculation overflowed")]
    AddressOverflow,
    /// Initializer table boundaries are inconsistent.
    #[error("per-CPU initializer table range {start:#x}..{end:#x} is malformed")]
    MalformedInitTable {
        /// First registration address.
        start: usize,
        /// One-past-the-end registration address.
        end: usize,
    },
    /// One typed initializer does not fit the template layout.
    #[error(
        "per-CPU initializer {index} has invalid offset {offset:#x}, size {size:#x}, or alignment \
         {alignment:#x}"
    )]
    MalformedInitRecord {
        /// Registration index in the final image.
        index: usize,
        /// Destination offset.
        offset: usize,
        /// Storage size.
        size: usize,
        /// Storage alignment.
        alignment: usize,
    },
    /// Two typed initializer destinations overlap.
    #[error("per-CPU initializer destinations overlap at {first_offset:#x} and {second_offset:#x}")]
    OverlappingInitRecords {
        /// First overlapping offset.
        first_offset: usize,
        /// Second overlapping offset.
        second_offset: usize,
    },
    /// Another initialization attempt is active.
    #[error("per-CPU layout initialization is already in progress")]
    LayoutInitializationInProgress,
    /// The one-shot layout has already been installed.
    #[error("per-CPU layout has already been initialized")]
    LayoutAlreadyInitialized,
    /// The target does not provide an ELF initializer table.
    #[error("per-CPU typed initializer table is unavailable on this target")]
    InitializerTableUnavailable,
    /// The CPU-local prefix is not first in the template.
    #[error(
        "per-CPU template base {template_base:#x} differs from prefix address {prefix_address:#x}"
    )]
    PrefixPlacement {
        /// Loaded template start.
        template_base: usize,
        /// Fixed prefix symbol address.
        prefix_address: usize,
    },
    /// No runtime layout has been installed.
    #[error("per-CPU runtime layout is not installed")]
    LayoutNotInstalled,
    /// Requested logical CPU is outside the installed region.
    #[error("CPU {cpu_index:?} is outside layout area count {area_count}")]
    CpuOutOfRange {
        /// Requested CPU.
        cpu_index: CpuIndex,
        /// Installed area count.
        area_count: u32,
    },
    /// A supplied current CPU area differs from the installed area.
    #[error("current CPU area {actual:?} differs from expected {expected:?}")]
    CurrentAreaMismatch {
        /// Area selected by the installed layout.
        expected: CpuAreaRef,
        /// Area carried by the pin.
        actual: CpuAreaRef,
    },
    /// CPU-local prefix construction or validation failed.
    #[error(transparent)]
    CpuLocal(#[from] CpuLocalError),
}
