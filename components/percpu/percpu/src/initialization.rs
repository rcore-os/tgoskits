//! One-shot construction of typed values at their final runtime addresses.

use core::{
    mem::size_of,
    sync::atomic::{AtomicU8, Ordering, compiler_fence},
};

use cpu_local::{CpuAreaPrefix, CpuIndex};

use crate::{
    PerCpuArea, PerCpuError, PerCpuLayout, PerCpuRegion,
    descriptor::{PerCpuInitRecord, PerCpuInitRegistration, init_registrations},
    layout::freeze_initialized_layout,
};

const UNINITIALIZED: u8 = 0;
const INITIALIZING: u8 = 1;
const INITIALIZED: u8 = 2;

static INITIALIZATION_STATE: AtomicU8 = AtomicU8::new(UNINITIALIZED);

/// Validates and constructs all values in raw runtime areas exactly once.
///
/// Every descriptor and overlap is checked before the first destination write.
/// The layout becomes globally visible only after all prefixes and objects are
/// fully initialized.
///
/// # Safety
///
/// `region` must name exclusively owned, writable, correctly aligned storage
/// that remains mapped until shutdown. No CPU may bind or access an area until
/// this function succeeds, and the bytes must not contain live Rust values.
pub unsafe fn initialize_layout(
    region: PerCpuRegion,
) -> Result<&'static PerCpuLayout, PerCpuError> {
    let candidate = PerCpuLayout::validate(region)?;
    begin_initialization()?;

    let registrations = match init_registrations() {
        Ok(registrations) => registrations,
        Err(error) => return reset_initialization(error),
    };
    if let Err(error) = validate_prefixes(&candidate)
        .and_then(|()| validate_init_records(registrations, &candidate))
    {
        return reset_initialization(error);
    }

    for cpu_raw in 0..candidate.area_count() {
        let cpu_index = CpuIndex::from_u32(cpu_raw)
            .expect("nonzero u32 area count must retain representable indices");
        let area = candidate
            .area(cpu_index)
            .expect("validated layout area must remain addressable");
        // SAFETY: caller ownership and complete preflight validation cover this
        // unique destination area. Each area is visited exactly once.
        unsafe { initialize_area(area, candidate.template_base(), registrations) };
    }

    compiler_fence(Ordering::Release);
    let installed = freeze_initialized_layout(candidate);
    INITIALIZATION_STATE.store(INITIALIZED, Ordering::Release);
    Ok(installed)
}

fn reset_initialization<T>(error: PerCpuError) -> Result<T, PerCpuError> {
    INITIALIZATION_STATE.store(UNINITIALIZED, Ordering::Release);
    Err(error)
}

fn begin_initialization() -> Result<(), PerCpuError> {
    match INITIALIZATION_STATE.compare_exchange(
        UNINITIALIZED,
        INITIALIZING,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(INITIALIZING) => Err(PerCpuError::LayoutInitializationInProgress),
        Err(INITIALIZED) => Err(PerCpuError::LayoutAlreadyInitialized),
        Err(_) => unreachable!("per-CPU initialization state must remain valid"),
    }
}

fn validate_prefixes(layout: &PerCpuLayout) -> Result<(), PerCpuError> {
    for cpu_raw in 0..layout.area_count() {
        let cpu_index = CpuIndex::from_u32(cpu_raw)
            .expect("validated area count must retain representable indices");
        let area = layout.area(cpu_index)?;
        CpuAreaPrefix::initialize(cpu_index, area.runtime_base())?;
    }
    Ok(())
}

fn validate_init_records(
    registrations: &[PerCpuInitRegistration],
    layout: &PerCpuLayout,
) -> Result<(), PerCpuError> {
    for (index, registration) in registrations.iter().copied().enumerate() {
        let record = registration.record(index, layout.template_base())?;
        validate_init_record(index, record, layout)?;
        for (relative, other) in registrations[index + 1..].iter().copied().enumerate() {
            let other_index = index + 1 + relative;
            let other_record = other.record(other_index, layout.template_base())?;
            if record.overlaps(other_record)? {
                return Err(PerCpuError::OverlappingInitRecords {
                    first_offset: record.offset,
                    second_offset: other_record.offset,
                });
            }
        }
    }
    Ok(())
}

fn validate_init_record(
    index: usize,
    record: PerCpuInitRecord,
    layout: &PerCpuLayout,
) -> Result<(), PerCpuError> {
    let end = record.end()?;
    if record.alignment == 0
        || !record.alignment.is_power_of_two()
        || record.offset < size_of::<CpuAreaPrefix>()
        || end > layout.area_size()
        || !record.offset.is_multiple_of(record.alignment)
        || record.alignment > layout.required_alignment()
    {
        return Err(PerCpuError::MalformedInitRecord {
            index,
            offset: record.offset,
            size: record.size,
            alignment: record.alignment,
        });
    }
    Ok(())
}

unsafe fn initialize_area(
    area: PerCpuArea,
    template_base: usize,
    registrations: &[PerCpuInitRegistration],
) {
    let prefix = CpuAreaPrefix::initialize(area.cpu_index(), area.runtime_base())
        .expect("preflighted prefix facts must remain valid");
    // SAFETY: caller owns this raw area and preflight reserves the prefix.
    unsafe { area.prefix_ptr().write(prefix) };
    for (index, registration) in registrations.iter().copied().enumerate() {
        let record = registration
            .record(index, template_base)
            .expect("validated descriptor must remain stable during initialization");
        // SAFETY: every non-overlapping record was validated before any write.
        let destination = unsafe { area.runtime_ptr().add(record.offset) };
        unsafe { (record.initialize)(destination) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::PerCpuInitRecord;

    unsafe extern "C" fn initialize_nothing(_destination: *mut u8) {}

    #[test]
    fn overlap_check_handles_empty_and_wrapping_records() {
        let record = |offset, size| PerCpuInitRecord {
            offset,
            size,
            alignment: 8,
            initialize: initialize_nothing,
        };
        assert_eq!(record(0x100, 0x20).overlaps(record(0x110, 8)), Ok(true));
        assert_eq!(record(0x100, 0x20).overlaps(record(0x110, 0)), Ok(false));
        assert_eq!(
            record(usize::MAX - 3, 8).end(),
            Err(PerCpuError::AddressOverflow)
        );
    }
}
