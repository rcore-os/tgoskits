use core::{fmt, marker::PhantomData};

/// Dense logical index assigned to one CPU-local area.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CpuIndex(u32);

impl CpuIndex {
    /// Reserved value used by an unbound CPU-area header.
    pub const INVALID_RAW: u32 = u32::MAX;

    /// Creates an index from its validated representation.
    pub const fn from_u32(index: u32) -> Option<Self> {
        if index == Self::INVALID_RAW {
            None
        } else {
            Some(Self(index))
        }
    }

    /// Returns the integer representation used at FFI and assembly boundaries.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns this index as a Rust collection index.
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<usize> for CpuIndex {
    type Error = CpuIndexError;

    fn try_from(index: usize) -> Result<Self, Self::Error> {
        let index = u32::try_from(index).map_err(|_| CpuIndexError { index })?;
        Self::from_u32(index).ok_or(CpuIndexError {
            index: index as usize,
        })
    }
}

/// Error returned when a logical CPU index does not fit the stable ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuIndexError {
    index: usize,
}

impl CpuIndexError {
    /// Returns the rejected index.
    pub const fn index(self) -> usize {
        self.index
    }
}

impl fmt::Display for CpuIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "CPU index {} exceeds the CPU-local ABI",
            self.index
        )
    }
}

impl core::error::Error for CpuIndexError {}

/// Runtime relocation applied to link-time addresses in the per-CPU section.
///
/// Zero is a valid relocation. It must never be used as an initialization
/// sentinel.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct PerCpuRelocation(usize);

impl PerCpuRelocation {
    /// Creates a relocation from its architecture-independent raw value.
    pub const fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// Calculates the relocation mapping `link_base` onto `runtime_base`.
    pub const fn from_bases(runtime_base: usize, link_base: usize) -> Self {
        Self(runtime_base.wrapping_sub(link_base))
    }

    /// Returns the raw relocation value.
    pub const fn raw(self) -> usize {
        self.0
    }

    /// Applies this relocation to a link-time address.
    pub const fn relocate(self, link_address: usize) -> usize {
        link_address.wrapping_add(self.0)
    }
}

/// Proof that the current execution context cannot migrate to another CPU.
///
/// `ax-kspin` owns the normal safe constructor by embedding this token in its
/// preemption guard. Early boot and architecture code may create it through
/// [`CpuPin::new_unchecked`] while migration is impossible by construction.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_cpu_local::CpuPin>();
/// ```
#[must_use = "the CPU may only be treated as pinned while this token is alive"]
#[derive(Debug)]
pub struct CpuPin {
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl CpuPin {
    /// Creates a CPU pin without changing scheduler state.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the current execution context cannot move
    /// to another CPU until the returned token is dropped. This token does not
    /// prove that an architecture anchor or CPU-local area is installed;
    /// higher layers must validate that state separately. Disabling only IRQs
    /// is sufficient during early boot before scheduling is possible, but is
    /// not sufficient in a running preemptible kernel.
    pub const unsafe fn new_unchecked() -> Self {
        Self {
            _not_send_or_sync: PhantomData,
        }
    }
}

/// Architecture installation value for one CPU-local area.
///
/// Architectures may encode either field in their hardware anchor. Consumers
/// must let the higher-level CPU-area owner interpret and validate the raw
/// register value rather than assuming one architecture's encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CpuLocalAnchor {
    area_base: usize,
    relocation: PerCpuRelocation,
}

impl CpuLocalAnchor {
    /// Describes an initialized runtime CPU-local area.
    pub const fn new(area_base: usize, relocation: PerCpuRelocation) -> Self {
        Self {
            area_base,
            relocation,
        }
    }

    /// Returns the runtime address of [`CpuAreaHeader`](crate::CpuAreaHeader).
    pub const fn area_base(self) -> usize {
        self.area_base
    }

    /// Returns the link-to-runtime relocation for this area.
    pub const fn relocation(self) -> PerCpuRelocation {
        self.relocation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocation_accepts_zero_and_wraps_like_linker_arithmetic() {
        assert_eq!(PerCpuRelocation::from_bases(0x1000, 0x1000).raw(), 0);
        assert_eq!(
            PerCpuRelocation::from_bases(0x2000, usize::MAX - 0xfff).relocate(usize::MAX - 0xfff),
            0x2000
        );
    }

    #[test]
    fn cpu_index_reserves_the_invalid_header_value() {
        assert_eq!(CpuIndex::try_from(7).unwrap().as_u32(), 7);
        assert!(CpuIndex::from_u32(CpuIndex::INVALID_RAW).is_none());
    }
}
