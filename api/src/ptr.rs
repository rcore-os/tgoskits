use core::{alloc::Layout, ffi::c_char, mem, slice, str};

use axerrno::{LinuxError, LinuxResult};
use axhal::paging::MappingFlags;
use axtask::{TaskExtRef, current};
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use starry_core::mm::access_user_memory;

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> LinuxResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(LinuxError::EFAULT);
    }

    let task = current();
    let mut aspace = task.task_ext().aspace.lock();

    if !aspace.check_region_access(
        VirtAddrRange::from_start_size(start, layout.size()),
        access_flags,
    ) {
        return Err(LinuxError::EFAULT);
    }

    let page_start = start.align_down_4k();
    let page_end = (start + layout.size()).align_up_4k();
    aspace.populate_area(page_start, page_end - page_start)?;

    Ok(())
}

fn check_null_terminated<T: Eq + Default>(
    start: VirtAddr,
    access_flags: MappingFlags,
) -> LinuxResult<(*const T, usize)> {
    let align = Layout::new::<T>().align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(LinuxError::EFAULT);
    }

    let zero = T::default();

    let mut page = start.align_down_4k();

    let start = start.as_ptr_of::<T>();
    let mut len = 0;

    access_user_memory(|| {
        loop {
            // SAFETY: This won't overflow the address space since we'll check
            // it below.
            let ptr = unsafe { start.add(len) };
            while ptr as usize >= page.as_ptr() as usize {
                // We cannot prepare `aspace` outside of the loop, since holding
                // aspace requires a mutex which would be required on page
                // fault, and page faults can trigger inside the loop.

                // TODO: this is inefficient, but we have to do this instead of
                // querying the page table since the page might has not been
                // allocated yet.
                let task = current();
                let aspace = task.task_ext().aspace.lock();
                if !aspace.check_region_access(
                    VirtAddrRange::from_start_size(page, PAGE_SIZE_4K),
                    access_flags,
                ) {
                    return Err(LinuxError::EFAULT);
                }

                page += PAGE_SIZE_4K;
            }

            // This might trigger a page fault
            // SAFETY: The pointer is valid and points to a valid memory region.
            if unsafe { ptr.read_volatile() } == zero {
                break;
            }
            len += 1;
        }
        Ok(())
    })?;

    Ok((start, len))
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

    fn nullable<R>(self, f: impl FnOnce(Self) -> LinuxResult<R>) -> LinuxResult<Option<R>> {
        if self.address().as_ptr().is_null() {
            Ok(None)
        } else {
            f(self).map(Some)
        }
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

impl<T> UserPtr<T> {
    /// Get the pointer as `&mut [T]`, terminated by a null value, validating
    /// the memory region.
    pub fn get_as_null_terminated(self) -> LinuxResult<&'static mut [T]>
    where
        T: Eq + Default,
    {
        let (ptr, len) = check_null_terminated::<T>(self.address(), Self::ACCESS_FLAGS)?;
        // SAFETY: We've validated the memory region.
        unsafe { Ok(slice::from_raw_parts_mut(ptr as *mut _, len)) }
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

impl<T> UserConstPtr<T> {
    /// Get the pointer as `&[T]`, terminated by a null value, validating the
    /// memory region.
    pub fn get_as_null_terminated(self) -> LinuxResult<&'static [T]>
    where
        T: Eq + Default,
    {
        let (ptr, len) = check_null_terminated::<T>(self.address(), Self::ACCESS_FLAGS)?;
        // SAFETY: We've validated the memory region.
        unsafe { Ok(slice::from_raw_parts(ptr, len)) }
    }
}

static_assertions::const_assert_eq!(size_of::<c_char>(), size_of::<u8>());

impl UserConstPtr<c_char> {
    /// Get the pointer as `&str`, validating the memory region.
    pub fn get_as_str(self) -> LinuxResult<&'static str> {
        let slice = self.get_as_null_terminated()?;
        // SAFETY: c_char is u8
        let slice = unsafe { mem::transmute::<&[c_char], &[u8]>(slice) };

        str::from_utf8(slice).map_err(|_| LinuxError::EILSEQ)
    }
}
