use axerrno::{LinuxError, LinuxResult};
use axhal::paging::{MappingFlags, PageTable};
use axtask::{TaskExtRef, current};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, VirtAddr};

use core::{alloc::Layout, ffi::CStr, slice};

fn check_page(pt: &PageTable, page: VirtAddr, access_flags: MappingFlags) -> LinuxResult<()> {
    let Ok((_, flags, _)) = pt.query(page) else {
        return Err(LinuxError::EFAULT);
    };
    if !flags.contains(access_flags) {
        return Err(LinuxError::EFAULT);
    }
    Ok(())
}

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> LinuxResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(LinuxError::EFAULT);
    }

    // TODO: currently we're doing a very basic and inefficient check, due to
    // the fact that AddrSpace does not expose necessary API.
    let task = current();
    let aspace = task.task_ext().aspace.lock();
    let pt = aspace.page_table();

    let page_start = start.align_down_4k();
    let page_end = (start + layout.size()).align_up_4k();
    for page in PageIter4K::new(page_start, page_end).unwrap() {
        check_page(pt, page, access_flags)?;
    }

    Ok(())
}

fn check_cstr(start: VirtAddr, access_flags: MappingFlags) -> LinuxResult<&'static CStr> {
    // TODO: see check_region
    let task = current();
    let aspace = task.task_ext().aspace.lock();
    let pt = aspace.page_table();

    let mut page = start.align_down_4k();
    check_page(pt, page, access_flags)?;
    page += PAGE_SIZE_4K;

    let start: *const u8 = start.as_ptr();
    let mut len = 0;

    loop {
        // SAFETY: Outer caller has provided a pointer to a valid C string.
        let ptr = unsafe { start.add(len) };
        if ptr == page.as_ptr() {
            check_page(pt, page, access_flags)?;
            page += PAGE_SIZE_4K;
        }

        // SAFETY: The pointer is valid and points to a valid memory region.
        if unsafe { *ptr } == 0 {
            break;
        }
        len += 1;
    }

    // SAFETY: We've checked that the memory region contains a valid C string.
    Ok(unsafe { CStr::from_bytes_with_nul_unchecked(slice::from_raw_parts(start, len + 1)) })
}

/// A trait representing a pointer in user space, which can be converted to a
/// pointer in kernel space through a series of checks.
///
/// Converting a `PtrWrapper<T>` to `*T` is done by `PtrWrapper::get` (or
/// `get_as_*`). It checks whether the pointer along with its layout is valid in
/// the current task's address space, and raises EFAULT if not.
pub trait PtrWrapper<T>: Sized {
    type Ptr;

    const ACCESS_FLAGS: MappingFlags;

    /// Unwrap the pointer to the inner type.
    ///
    /// This function is unsafe because it assumes that the pointer is valid and
    /// points to a valid memory region.
    unsafe fn into_inner(self) -> Self::Ptr;

    /// Get the address of the pointer.
    fn address(&self) -> VirtAddr;

    /// Get the pointer as a raw pointer to `T`.
    fn get(self) -> LinuxResult<Self::Ptr> {
        self.get_as(Layout::new::<T>())
    }

    /// Get the pointer as a raw pointer to `T`, validating the memory
    /// region given by the layout.
    fn get_as(self, layout: Layout) -> LinuxResult<Self::Ptr> {
        check_region(self.address(), layout, Self::ACCESS_FLAGS)?;
        unsafe { Ok(self.into_inner()) }
    }

    /// Get the pointer as a raw pointer to `T`, validating the memory
    /// region specified by the size.
    fn get_as_bytes(self, size: usize) -> LinuxResult<Self::Ptr> {
        check_region(
            self.address(),
            Layout::from_size_align(size, 1).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        unsafe { Ok(self.into_inner()) }
    }

    /// Get the pointer as a raw pointer to `T`, validating the memory
    /// region given by the layout of `[T; len]`.
    fn get_as_array(self, len: usize) -> LinuxResult<Self::Ptr> {
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        unsafe { Ok(self.into_inner()) }
    }

    /// Get the pointer as `&CStr`, validating the memory region specified by
    /// the size of a C string.
    fn get_as_cstr(self) -> LinuxResult<&'static CStr> {
        check_cstr(self.address(), Self::ACCESS_FLAGS)
    }
}

/// A pointer to user space memory.
///
/// See [`PtrWrapper`] for more details.
#[repr(transparent)]
pub struct UserPtr<T>(*mut T);

impl<T> From<usize> for UserPtr<T> {
    fn from(value: usize) -> Self {
        UserPtr(value as *mut _)
    }
}

impl<T> PtrWrapper<T> for UserPtr<T> {
    type Ptr = *mut T;

    const ACCESS_FLAGS: MappingFlags = MappingFlags::READ.union(MappingFlags::WRITE);

    unsafe fn into_inner(self) -> Self::Ptr {
        self.0
    }

    fn address(&self) -> VirtAddr {
        VirtAddr::from_mut_ptr_of(self.0)
    }
}

/// An immutable pointer to user space memory.
///
/// See [`PtrWrapper`] for more details.
#[repr(transparent)]
pub struct UserConstPtr<T>(*const T);

impl<T> From<usize> for UserConstPtr<T> {
    fn from(value: usize) -> Self {
        UserConstPtr(value as *const _)
    }
}

impl<T> PtrWrapper<T> for UserConstPtr<T> {
    type Ptr = *const T;

    const ACCESS_FLAGS: MappingFlags = MappingFlags::READ;

    unsafe fn into_inner(self) -> Self::Ptr {
        self.0
    }

    fn address(&self) -> VirtAddr {
        VirtAddr::from_ptr_of(self.0)
    }
}
