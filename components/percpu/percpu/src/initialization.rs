//! Final-address construction of values in externally reserved CPU areas.

use core::{
    mem::{align_of, size_of},
    sync::atomic::{AtomicU8, Ordering, compiler_fence},
};

use crate::{
    CpuAreaPrefixV2, PerCpuArea, PerCpuError, PerCpuLayoutInitV2,
    area::{InstalledLayout, freeze_initialized_layout},
};

const UNINITIALIZED: u8 = 0;
const INITIALIZING: u8 = 1;
const INITIALIZED: u8 = 2;

/// Success returned by [`__ax_percpu_initialize_layout_v2`].
#[doc(hidden)]
pub const FFI_INIT_OK: u32 = 0;
/// Failure returned by [`__ax_percpu_initialize_layout_v2`].
#[doc(hidden)]
pub const FFI_INIT_FAILED: u32 = 1;

static INITIALIZATION_STATE: AtomicU8 = AtomicU8::new(UNINITIALIZED);

type PerCpuInitializer = unsafe extern "C" fn(*mut u8);
type PerCpuDescriptorThunk = unsafe extern "C" fn() -> PerCpuInitDescriptor;

/// Loaded-image description returned by one macro-generated thunk.
///
/// `storage_address` is observed only after final relocation. The semantic
/// layer immediately converts it to a checked template-relative scalar before
/// accepting the corresponding [`PerCpuInitRecord`].
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PerCpuInitDescriptor {
    storage_address: usize,
    size: usize,
    alignment: usize,
    initialize: PerCpuInitializer,
}

impl PerCpuInitDescriptor {
    /// Creates a loaded-image descriptor returned by generated code.
    ///
    /// # Safety
    ///
    /// `storage_address`, `size`, and `alignment` must describe the exact
    /// `MaybeUninit<Storage>` object owned by this descriptor in the final
    /// per-CPU template. `initialize` must construct exactly one valid
    /// `Storage` value at that destination without reading or dropping its
    /// previous uninitialized bytes.
    #[doc(hidden)]
    pub const unsafe fn new(
        storage_address: usize,
        size: usize,
        alignment: usize,
        initialize: PerCpuInitializer,
    ) -> Self {
        Self {
            storage_address,
            size,
            alignment,
            initialize,
        }
    }

    fn resolve(self, index: usize, template_base: usize) -> Result<PerCpuInitRecord, PerCpuError> {
        let offset = self.storage_address.checked_sub(template_base).ok_or(
            PerCpuError::MalformedInitRecord {
                index,
                offset: self.storage_address,
                size: self.size,
                alignment: self.alignment,
            },
        )?;
        Ok(PerCpuInitRecord::new(
            offset,
            self.size,
            self.alignment,
            self.initialize,
        ))
    }
}

/// Final-image description of one typed CPU-local storage object.
///
/// The descriptor thunk constructs this value only after the loader has
/// finished relocating the image. `offset`, `size`, and `alignment` are plain
/// relative scalars; the initializer pointer is consumed in place and is never
/// copied into a CPU area.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct PerCpuInitRecord {
    offset: usize,
    size: usize,
    alignment: usize,
    initialize: PerCpuInitializer,
}

impl PerCpuInitRecord {
    const fn new(
        offset: usize,
        size: usize,
        alignment: usize,
        initialize: PerCpuInitializer,
    ) -> Self {
        Self {
            offset,
            size,
            alignment,
            initialize,
        }
    }

    fn end(self) -> Result<usize, PerCpuError> {
        self.offset
            .checked_add(self.size)
            .ok_or(PerCpuError::AddressOverflow)
    }

    fn overlaps(self, other: Self) -> Result<bool, PerCpuError> {
        if self.size == 0 || other.size == 0 {
            return Ok(false);
        }
        Ok(self.offset < other.end()? && other.offset < self.end()?)
    }
}

/// Linker-retained registration for one descriptor thunk.
///
/// A function pointer is necessary because Rust const evaluation treats two
/// statics as distinct allocations and cannot encode their linker-established
/// address difference as an integer. The final-high descriptor resolves that
/// difference after relocation, before any CPU-area value is written.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct PerCpuInitRegistration {
    describe: PerCpuDescriptorThunk,
}

impl PerCpuInitRegistration {
    /// Creates one macro-generated registration.
    ///
    /// # Safety
    ///
    /// `describe` must remain valid for the final image lifetime and return
    /// the same descriptor on every invocation. Its storage address and
    /// initializer must belong to that same final image and satisfy
    /// [`PerCpuInitDescriptor::new`]'s contract. Initialization relies on this
    /// determinism when it validates all records before performing any write.
    #[doc(hidden)]
    pub const unsafe fn new(describe: PerCpuDescriptorThunk) -> Self {
        Self { describe }
    }

    fn record(self, index: usize, template_base: usize) -> Result<PerCpuInitRecord, PerCpuError> {
        // SAFETY: registrations are generated by def_percpu and retained as an
        // immutable table in the same final image as their descriptor thunks.
        unsafe { (self.describe)() }.resolve(index, template_base)
    }
}

/// Validates and constructs all values in a raw CPU-area layout exactly once.
///
/// All initializer descriptors, ranges, and pairwise overlaps are checked
/// before the first destination write. The layout becomes globally visible
/// only after every area has received its prefix and every typed value.
///
/// # Safety
///
/// The complete layout must name exclusively owned, writable, correctly
/// aligned raw storage that remains mapped for the kernel lifetime. No CPU may
/// bind an area, and no current or remote per-CPU access may occur, until this
/// function succeeds. Each byte that may contain a previous Rust value must be
/// uninitialized; this function does not drop overwritten contents.
pub unsafe fn initialize_layout(init: PerCpuLayoutInitV2) -> Result<usize, PerCpuError> {
    let layout = init.layout();
    let candidate = InstalledLayout::from_public(layout, init.identity())?;
    begin_initialization()?;

    let registrations = match init_registrations() {
        Ok(registrations) => registrations,
        Err(error) => {
            INITIALIZATION_STATE.store(UNINITIALIZED, Ordering::Release);
            return Err(error);
        }
    };
    if let Err(error) =
        validate_prefixes(candidate).and_then(|()| validate_init_records(registrations, candidate))
    {
        INITIALIZATION_STATE.store(UNINITIALIZED, Ordering::Release);
        return Err(error);
    }

    for cpu_raw in 0..layout.area_count {
        let cpu_index = crate::CpuIndex::from_u32(cpu_raw)
            .expect("validated CPU-area count must retain representable indices");
        let area = candidate
            .area(cpu_index)
            .expect("validated layout area must remain addressable");
        // SAFETY: caller ownership and complete record validation cover this
        // unique destination area. The loop visits each area exactly once.
        unsafe { initialize_area(area, candidate.template_base(), registrations) };
    }

    compiler_fence(Ordering::Release);
    freeze_initialized_layout(candidate);
    INITIALIZATION_STATE.store(INITIALIZED, Ordering::Release);
    Ok(layout.area_count as usize)
}

/// Returns the register-ownership mode selected by this final image.
///
/// Boot code uses this scalar query before supplying the complete facts to
/// [`__ax_percpu_initialize_layout_v2`]. Keeping the mode explicit prevents a
/// platform ABI from silently overloading the v1 layout flags.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn __ax_percpu_image_register_mode_v1() -> u8 {
    cpu_local::image_register_mode().as_u8()
}

/// Value-only ABI used by someboot after the final high-address relocation.
///
/// A nonzero status is fatal to the caller. Detailed typed errors remain on
/// the Rust API because no references or Rust enum layout cross this boundary.
///
/// # Safety
///
/// The scalar layout must satisfy [`initialize_layout`]'s raw-storage and
/// lifetime contract.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __ax_percpu_initialize_layout_v2(
    runtime_base: usize,
    area_stride: usize,
    area_count: u32,
    flags: u32,
    abi_version: u16,
    register_mode: u8,
    host_level: u8,
    generation: u32,
    cookie: usize,
) -> u32 {
    let init = PerCpuLayoutInitV2 {
        runtime_base,
        area_stride,
        area_count,
        flags,
        abi_version,
        register_mode,
        host_level,
        generation,
        cookie,
    };
    // SAFETY: forwarded value-only ABI contract matches initialize_layout.
    match unsafe { initialize_layout(init) } {
        Ok(_) => FFI_INIT_OK,
        Err(_) => FFI_INIT_FAILED,
    }
}

fn validate_prefixes(layout: InstalledLayout) -> Result<(), PerCpuError> {
    for cpu_raw in 0..layout.public().area_count {
        let cpu_index = crate::CpuIndex::from_u32(cpu_raw)
            .expect("validated CPU-area count must retain representable indices");
        let area = layout
            .area(cpu_index)
            .expect("validated layout area must remain addressable");
        CpuAreaPrefixV2::initialize(area.init_facts()).map_err(PerCpuError::Prefix)?;
    }
    Ok(())
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
        Err(_) => unreachable!("CPU-local initialization state must remain valid"),
    }
}

fn validate_init_records(
    registrations: &[PerCpuInitRegistration],
    layout: InstalledLayout,
) -> Result<(), PerCpuError> {
    for (index, registration) in registrations.iter().copied().enumerate() {
        let record = registration.record(index, layout.template_base())?;
        validate_init_record(index, record, layout)?;
        for (other_index, other) in registrations[index + 1..].iter().copied().enumerate() {
            let other_index = index + 1 + other_index;
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
    layout: InstalledLayout,
) -> Result<(), PerCpuError> {
    if record.alignment == 0 || !record.alignment.is_power_of_two() {
        return Err(PerCpuError::MalformedInitRecord {
            index,
            offset: record.offset,
            size: record.size,
            alignment: record.alignment,
        });
    }
    let end = record.end()?;
    if record.offset < size_of::<CpuAreaPrefixV2>()
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
    // SAFETY: caller owns the raw aligned area and validation reserves the
    // prefix range exclusively for CpuAreaPrefixV2. Prefix construction was
    // preflighted for every CPU before this first destination write.
    let prefix = CpuAreaPrefixV2::initialize(area.init_facts())
        .expect("preflighted CPU-area prefix facts must remain valid");
    unsafe { area.prefix_ptr().write(prefix) };
    for (index, registration) in registrations.iter().copied().enumerate() {
        let record = registration
            .record(index, template_base)
            .expect("validated CPU-local descriptor must remain stable during initialization");
        // SAFETY: the complete table was validated before any area write. This
        // area is exclusive and each non-overlapping destination is visited
        // exactly once.
        let destination = unsafe { area.runtime_ptr().add(record.offset) };
        unsafe { (record.initialize)(destination) };
    }
}

#[cfg(not(target_os = "macos"))]
fn init_registrations() -> Result<&'static [PerCpuInitRegistration], PerCpuError> {
    unsafe extern "C" {
        static __AX_PERCPU_INIT_START: PerCpuInitRegistration;
        static __AX_PERCPU_INIT_END: PerCpuInitRegistration;
    }

    let start = core::ptr::addr_of!(__AX_PERCPU_INIT_START) as usize;
    let end = core::ptr::addr_of!(__AX_PERCPU_INIT_END) as usize;
    let byte_len = end
        .checked_sub(start)
        .ok_or(PerCpuError::MalformedInitTable { start, end })?;
    if !start.is_multiple_of(align_of::<PerCpuInitRegistration>())
        || !byte_len.is_multiple_of(size_of::<PerCpuInitRegistration>())
    {
        return Err(PerCpuError::MalformedInitTable { start, end });
    }
    let record_count = byte_len / size_of::<PerCpuInitRegistration>();
    // SAFETY: the linker bounds were checked for order, element alignment, and
    // exact element size. KEEP retains only immutable registration statics in
    // this range for the final image lifetime.
    Ok(
        unsafe {
            core::slice::from_raw_parts(start as *const PerCpuInitRegistration, record_count)
        },
    )
}

#[cfg(target_os = "macos")]
fn init_registrations() -> Result<&'static [PerCpuInitRegistration], PerCpuError> {
    Err(PerCpuError::InitializerTableUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn initialize_nothing(_destination: *mut u8) {}

    #[test]
    fn descriptor_before_template_is_rejected_without_pointer_subtraction_wrap() {
        // SAFETY: this test never invokes the initializer and constructs the
        // malformed descriptor solely to exercise checked offset rejection.
        let descriptor = unsafe { PerCpuInitDescriptor::new(0x0fff, 8, 8, initialize_nothing) };
        assert!(matches!(
            descriptor.resolve(3, 0x1000),
            Err(PerCpuError::MalformedInitRecord {
                index: 3,
                offset: 0x0fff,
                size: 8,
                alignment: 8,
            })
        ));
    }

    #[test]
    fn overlap_check_handles_zero_sized_and_wrapping_records() {
        let first = PerCpuInitRecord::new(0x100, 0x20, 8, initialize_nothing);
        let overlapping = PerCpuInitRecord::new(0x110, 8, 8, initialize_nothing);
        let empty = PerCpuInitRecord::new(0x110, 0, 8, initialize_nothing);
        let wrapping = PerCpuInitRecord::new(usize::MAX - 3, 8, 8, initialize_nothing);

        assert_eq!(first.overlaps(overlapping), Ok(true));
        assert_eq!(first.overlaps(empty), Ok(false));
        assert_eq!(wrapping.end(), Err(PerCpuError::AddressOverflow));
    }
}
