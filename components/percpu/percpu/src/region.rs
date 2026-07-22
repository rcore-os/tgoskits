use core::{num::NonZeroU32, ptr::NonNull};

/// Platform-owned raw storage geometry for runtime per-CPU areas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCpuRegion {
    runtime_base: NonNull<u8>,
    area_stride: usize,
    area_count: NonZeroU32,
}

// SAFETY: this value describes externally synchronized permanent storage; it
// does not itself grant access to any bytes.
unsafe impl Send for PerCpuRegion {}
// SAFETY: see Send; shared geometry is immutable.
unsafe impl Sync for PerCpuRegion {}

impl PerCpuRegion {
    /// Creates raw runtime geometry to be validated by `initialize_layout`.
    pub const fn new(
        runtime_base: NonNull<u8>,
        area_stride: usize,
        area_count: NonZeroU32,
    ) -> Self {
        Self {
            runtime_base,
            area_stride,
            area_count,
        }
    }

    /// Returns CPU zero's runtime base.
    pub fn runtime_base(self) -> usize {
        self.runtime_base.as_ptr() as usize
    }

    /// Returns the byte stride between adjacent areas.
    pub const fn area_stride(self) -> usize {
        self.area_stride
    }

    /// Returns the number of runtime areas.
    pub const fn area_count(self) -> NonZeroU32 {
        self.area_count
    }
}
