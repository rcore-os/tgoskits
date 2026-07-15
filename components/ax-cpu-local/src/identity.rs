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

    /// Returns the integer representation used at ABI boundaries.
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
        let raw = u32::try_from(index).map_err(|_| CpuIndexError { index })?;
        Self::from_u32(raw).ok_or(CpuIndexError { index })
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

/// Proof that the current execution context cannot migrate to another CPU.
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
    /// The caller must prevent migration until this token is dropped. Early
    /// boot may use this while the CPU is offline; normal code obtains it from
    /// an IRQ/preemption guard.
    pub const unsafe fn new_unchecked() -> Self {
        Self {
            _not_send_or_sync: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_index_reserves_the_invalid_header_value() {
        assert_eq!(CpuIndex::try_from(7).unwrap().as_u32(), 7);
        assert!(CpuIndex::from_u32(CpuIndex::INVALID_RAW).is_none());
    }
}
