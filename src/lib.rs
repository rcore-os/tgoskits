//! Utilities for working with user-space pointers.

use axerrno::{LinuxError, LinuxResult};
use axhal::paging::MappingFlags;
use axmm::AddrSpace;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange};

use core::{alloc::Layout, ffi::c_char, mem, slice, str};

#[percpu::def_percpu]
static mut ACCESSING_USER_MEM: bool = false;

/// Check if we are currently accessing user memory.
///
/// OS implementation shall allow page faults from kernel when this function
/// returns true.
pub fn is_accessing_user_memory() -> bool {
    ACCESSING_USER_MEM.read_current()
}

fn access_user_memory<R>(f: impl FnOnce() -> R) -> R {
    ACCESSING_USER_MEM.with_current(|v| {
        *v = true;
        let result = f();
        *v = false;
        result
    })
}

fn check_region(
    aspace: &mut AddrSpace,
    start: VirtAddr,
    layout: Layout,
    access_flags: MappingFlags,
) -> LinuxResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(LinuxError::EFAULT);
    }

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
    aspace: &mut AddrSpace,
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

#[repr(transparent)]
pub struct UserPtr<T>(*mut T);
impl<T> From<usize> for UserPtr<T> {
    fn from(value: usize) -> Self {
        UserPtr(value as *mut T)
    }
}

impl<T> UserPtr<T> {
    pub const ACCESS_FLAGS: MappingFlags = MappingFlags::READ.union(MappingFlags::WRITE);

    /// Get the address of the pointer.
    pub fn address(&self) -> VirtAddr {
        VirtAddr::from_mut_ptr_of(self.0)
    }

    /// Unwrap the pointer into a raw pointer.
    ///
    /// This function is unsafe because it assumes that the pointer is valid and
    /// points to a valid memory region.
    pub unsafe fn as_ptr(&self) -> *mut T {
        self.0
    }

    /// Cast the pointer to a different type.
    pub fn cast<U>(self) -> UserPtr<U> {
        UserPtr(self.0 as *mut U)
    }

    /// Check if the pointer is null.
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// Convert the pointer into an `Option`.
    ///
    /// This function returns `None` if the pointer is null, and `Some(self)`
    /// otherwise.
    pub fn nullable(self) -> Option<Self> {
        if self.is_null() { None } else { Some(self) }
    }
}

impl<T> UserPtr<T> {
    /// Get the value of the pointer.
    ///
    /// This will check the region and populate the page if necessary.
    pub fn get<'a>(&'a mut self, aspace: &mut AddrSpace) -> LinuxResult<&'a mut T> {
        check_region(
            aspace,
            self.address(),
            Layout::new::<T>(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { &mut *self.0 })
    }

    /// Get the value of the pointer as a slice.
    pub fn get_as_slice<'a>(
        &'a mut self,
        aspace: &mut AddrSpace,
        length: usize,
    ) -> LinuxResult<&'a mut [T]> {
        check_region(
            aspace,
            self.address(),
            Layout::array::<T>(length).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts_mut(self.0, length) })
    }
}

impl<T> UserPtr<T> {
    /// Get the pointer as `&mut [T]`, terminated by a null value, validating
    /// the memory region.
    pub fn get_as_null_terminated(&mut self, aspace: &mut AddrSpace) -> LinuxResult<&mut [T]>
    where
        T: Eq + Default,
    {
        let (ptr, len) = check_null_terminated::<T>(aspace, self.address(), Self::ACCESS_FLAGS)?;
        // SAFETY: We've validated the memory region.
        unsafe { Ok(slice::from_raw_parts_mut(ptr as *mut _, len)) }
    }
}

#[repr(transparent)]
pub struct UserConstPtr<T>(*const T);
impl<T> From<usize> for UserConstPtr<T> {
    fn from(value: usize) -> Self {
        UserConstPtr(value as *const T)
    }
}

impl<T> UserConstPtr<T> {
    pub const ACCESS_FLAGS: MappingFlags = MappingFlags::READ;

    /// See [`UserPtr::address`].
    pub fn address(&self) -> VirtAddr {
        VirtAddr::from_ptr_of(self.0)
    }

    /// See [`UserPtr::as_ptr`].
    pub unsafe fn as_ptr(&self) -> *const T {
        self.0
    }

    /// See [`UserPtr::cast`].
    pub fn cast<U>(self) -> UserConstPtr<U> {
        UserConstPtr(self.0 as *const U)
    }

    /// See [`UserPtr::is_null`].
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// See [`UserPtr::nullable`].
    pub fn nullable(self) -> Option<Self> {
        if self.is_null() { None } else { Some(self) }
    }
}

impl<T> UserConstPtr<T> {
    /// See [`UserPtr::get`].
    pub fn get<'a>(&'a self, aspace: &mut AddrSpace) -> LinuxResult<&'a T> {
        check_region(
            aspace,
            self.address(),
            Layout::new::<T>(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { &*self.0 })
    }

    /// See [`UserPtr::get_as_slice`].
    pub fn get_as_slice<'a>(
        &'a self,
        aspace: &mut AddrSpace,
        length: usize,
    ) -> LinuxResult<&'a [T]> {
        check_region(
            aspace,
            self.address(),
            Layout::array::<T>(length).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts(self.0, length) })
    }
}

impl<T> UserConstPtr<T> {
    /// See [`UserPtr::get_as_null_terminated`].
    pub fn get_as_null_terminated(&self, aspace: &mut AddrSpace) -> LinuxResult<&[T]>
    where
        T: Eq + Default,
    {
        let (ptr, len) = check_null_terminated::<T>(aspace, self.address(), Self::ACCESS_FLAGS)?;
        // SAFETY: We've validated the memory region.
        unsafe { Ok(slice::from_raw_parts(ptr as *const _, len)) }
    }
}

impl UserConstPtr<c_char> {
    /// Get the pointer as `&str`, validating the memory region.
    pub fn get_as_str(&self, aspace: &mut AddrSpace) -> LinuxResult<&'static str> {
        let slice = self.get_as_null_terminated(aspace)?;
        // SAFETY: c_char is u8
        let slice = unsafe { mem::transmute::<&[c_char], &[u8]>(slice) };

        str::from_utf8(slice).map_err(|_| LinuxError::EILSEQ)
    }
}
