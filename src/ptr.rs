use axerrno::{LinuxError, LinuxResult};
use axtask::{TaskExtRef, current};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, PageIter4K, VirtAddr};

use core::{alloc::Layout, ffi::c_char};

fn check_region(start: VirtAddr, layout: Layout) -> LinuxResult<()> {
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
        if pt.query(page).is_err() {
            return Err(LinuxError::EFAULT);
        }
    }

    Ok(())
}

fn check_cstr(start: VirtAddr) -> LinuxResult<()> {
    // TODO: see check_region
    let task = current();
    let aspace = task.task_ext().aspace.lock();
    let pt = aspace.page_table();

    let mut it = start;
    let mut page = it.align_down_4k();
    if pt.query(page).is_err() {
        return Err(LinuxError::EFAULT);
    }
    page += PAGE_SIZE_4K;
    loop {
        if unsafe { *it.as_ptr_of::<c_char>() } == 0 {
            break;
        }

        it += 1;
        if it == page {
            if pt.query(page).is_err() {
                return Err(LinuxError::EFAULT);
            }
            page += PAGE_SIZE_4K;
        }
    }

    Ok(())
}

/// A trait representing a pointer in user space, which can be converted to a
/// pointer in kernel space through a series of checks.
///
/// Converting a `PtrWrapper<T>` to `*T` is done by `PtrWrapper::get` (or
/// `get_as_*`). It checks whether the pointer along with its layout is valid in
/// the current task's address space, and raises EFAULT if not.
pub trait PtrWrapper<T>: Sized {
    type Ptr;

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
        check_region(self.address(), layout)?;
        unsafe { Ok(self.into_inner()) }
    }

    /// Get the pointer as a raw pointer to `T`, validating the memory
    /// region specified by the size.
    fn get_as_bytes(self, size: usize) -> LinuxResult<Self::Ptr> {
        check_region(self.address(), Layout::from_size_align(size, 1).unwrap())?;
        unsafe { Ok(self.into_inner()) }
    }

    /// Get the pointer as a raw pointer to `T`, validating the memory
    /// region given by the layout of `[T; len]`.
    fn get_as_array(self, len: usize) -> LinuxResult<Self::Ptr> {
        check_region(self.address(), Layout::array::<T>(len).unwrap())?;
        unsafe { Ok(self.into_inner()) }
    }

    /// Get the pointer as a raw pointer to `T`, validating the memory
    /// region specified by the size of a C string.
    fn get_as_cstr(self) -> LinuxResult<Self::Ptr> {
        check_cstr(self.address())?;
        unsafe { Ok(self.into_inner()) }
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

    unsafe fn into_inner(self) -> Self::Ptr {
        self.0
    }

    fn address(&self) -> VirtAddr {
        VirtAddr::from_ptr_of(self.0)
    }
}
