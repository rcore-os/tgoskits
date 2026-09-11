//! Shared validation and the two x86 MSR bitmap encodings.

use super::{Backend, memory::ControlRegion};
use crate::virtualization::{ControlMemory, VirtualizationError};

pub(super) fn port_range(
    first: u16,
    count: u32,
) -> Result<core::ops::Range<usize>, VirtualizationError> {
    let start = usize::from(first);
    let end = start
        .checked_add(count as usize)
        .filter(|end| *end <= 65536)
        .ok_or(VirtualizationError::InvalidPortRange)?;
    Ok(start..end)
}

pub(super) fn msr_bit(
    backend: Backend,
    msr: u32,
    write: bool,
) -> Result<usize, VirtualizationError> {
    let segment = match msr {
        0..=0x1fff => 0,
        0xc000_0000..=0xc000_1fff => 1,
        0xc001_0000..=0xc001_1fff if backend == Backend::Svm => 2,
        _ => return Err(VirtualizationError::UnsupportedMsr),
    };
    let index = (msr & 0x1fff) as usize;
    Ok(match backend {
        Backend::Vmx => (segment + 2 * usize::from(write)) * 8192 + index,
        Backend::Svm => segment * 16384 + 2 * index + usize::from(write),
    })
}

pub(super) fn set_msr<M: ControlMemory>(
    region: &mut ControlRegion<M>,
    backend: Backend,
    msr: u32,
    write: bool,
    intercept: bool,
) -> Result<(), VirtualizationError> {
    let bit = msr_bit(backend, msr, write)?;
    region.set_bit(bit, intercept);
    Ok(())
}
