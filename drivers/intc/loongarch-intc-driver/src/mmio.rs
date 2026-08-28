//! Shared validation for typed MMIO register blocks.

use core::mem::{align_of, size_of};

use mmio_api::MmioRaw;

use crate::IntcError;

pub(crate) fn validate_mmio_region<T>(
    mmio: &MmioRaw,
    region: &'static str,
) -> Result<(), IntcError> {
    let required = size_of::<T>();
    if mmio.size() < required {
        return Err(IntcError::MmioTooSmall {
            region,
            actual: mmio.size(),
            required,
        });
    }

    let required_alignment = align_of::<T>();
    if mmio.as_ptr().align_offset(required_alignment) != 0 {
        return Err(IntcError::MmioMisaligned {
            region,
            address: mmio.as_ptr() as usize,
            required_alignment,
        });
    }
    Ok(())
}
