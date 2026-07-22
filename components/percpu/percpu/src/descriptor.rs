//! Final-image typed initializer descriptors.

use core::mem::{align_of, size_of};

use crate::PerCpuError;

pub(crate) type PerCpuInitializer = unsafe extern "C" fn(*mut u8);
type PerCpuDescriptorThunk = unsafe extern "C" fn() -> PerCpuInitDescriptor;

/// Loaded-image description returned by one macro-generated thunk.
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
    /// The address, size, alignment, and writer must describe the exact
    /// generated `MaybeUninit<Storage>` object in this final image.
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
        Ok(PerCpuInitRecord {
            offset,
            size: self.size,
            alignment: self.alignment,
            initialize: self.initialize,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PerCpuInitRecord {
    pub(crate) offset: usize,
    pub(crate) size: usize,
    pub(crate) alignment: usize,
    pub(crate) initialize: PerCpuInitializer,
}

impl PerCpuInitRecord {
    pub(crate) fn end(self) -> Result<usize, PerCpuError> {
        self.offset
            .checked_add(self.size)
            .ok_or(PerCpuError::AddressOverflow)
    }

    pub(crate) fn overlaps(self, other: Self) -> Result<bool, PerCpuError> {
        if self.size == 0 || other.size == 0 {
            return Ok(false);
        }
        Ok(self.offset < other.end()? && other.offset < self.end()?)
    }
}

/// Linker-retained registration for one descriptor thunk.
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
    /// The thunk must remain deterministic and final-image resident.
    #[doc(hidden)]
    pub const unsafe fn new(describe: PerCpuDescriptorThunk) -> Self {
        Self { describe }
    }

    pub(crate) fn record(
        self,
        index: usize,
        template_base: usize,
    ) -> Result<PerCpuInitRecord, PerCpuError> {
        // SAFETY: def_percpu owns every retained immutable registration.
        unsafe { (self.describe)() }.resolve(index, template_base)
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn init_registrations() -> Result<&'static [PerCpuInitRegistration], PerCpuError> {
    unsafe extern "C" {
        static __PERCPU_INIT_START: PerCpuInitRegistration;
        static __PERCPU_INIT_END: PerCpuInitRegistration;
    }
    let start = core::ptr::addr_of!(__PERCPU_INIT_START) as usize;
    let end = core::ptr::addr_of!(__PERCPU_INIT_END) as usize;
    let byte_len = end
        .checked_sub(start)
        .ok_or(PerCpuError::MalformedInitTable { start, end })?;
    if !start.is_multiple_of(align_of::<PerCpuInitRegistration>())
        || !byte_len.is_multiple_of(size_of::<PerCpuInitRegistration>())
    {
        return Err(PerCpuError::MalformedInitTable { start, end });
    }
    // SAFETY: linker bounds were checked for order, alignment, and exact size.
    Ok(unsafe {
        core::slice::from_raw_parts(
            start as *const PerCpuInitRegistration,
            byte_len / size_of::<PerCpuInitRegistration>(),
        )
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn init_registrations() -> Result<&'static [PerCpuInitRegistration], PerCpuError> {
    Err(PerCpuError::InitializerTableUnavailable)
}
