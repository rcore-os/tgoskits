//! LoongArch virtual-address geometry derived from CPUCFG.

/// Maximum canonical-half exponent supported by the configured four-level
/// 4-KiB page-table walker.
pub const PAGE_TABLE_VA_BITS: usize = 47;

/// Smallest VALEN that can describe one 4-KiB page in each canonical half.
const MIN_VALEN: usize = 13;

/// A CPUCFG virtual-address width unsupported by this page-table build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsupportedValen {
    /// Architectural VALEN reported by CPUCFG1.
    pub valen: usize,
}

/// The page-table-backed lower and upper canonical halves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoongArchVirtualAddressLayout {
    lower_end: usize,
    upper_start: usize,
}

impl LoongArchVirtualAddressLayout {
    /// Builds the layout from the architectural VALEN value returned by the
    /// `loongArch64` CPUCFG wrapper.
    ///
    /// CPUCFG1 stores `VALEN - 1`. The wrapper adds one, while Linux retains
    /// the encoded exponent as `cpu_vabits`; therefore both implementations
    /// split the address space at `1 << (valen - 1)`.
    pub const fn from_valen(valen: usize) -> Result<Self, UnsupportedValen> {
        if valen < MIN_VALEN || valen > PAGE_TABLE_VA_BITS + 1 {
            return Err(UnsupportedValen { valen });
        }
        let canonical_half_bits = valen - 1;
        let half_size = 1usize << canonical_half_bits;
        Ok(Self {
            lower_end: half_size,
            upper_start: 0usize.wrapping_sub(half_size),
        })
    }

    /// Exclusive upper bound of the lower canonical half.
    pub const fn lower_end(self) -> usize {
        self.lower_end
    }

    /// Inclusive start of the upper canonical half.
    pub const fn upper_start(self) -> usize {
        self.upper_start
    }
}
