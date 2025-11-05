use alloc::string::String;
use core::{
    alloc::Layout,
    ffi::c_char,
    hint::unlikely,
    mem::{MaybeUninit, transmute},
    ptr, slice, str,
};

use axerrno::{AxError, AxResult};
use axhal::{
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};
use axio::{Buf, BufMut, Read, Write};
use axtask::current;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use starry_core::{
    mm::{access_user_memory, is_accessing_user_memory},
    task::AsThread,
};
use starry_vm::{vm_load_until_nul, vm_read_slice, vm_write_slice};

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> AxResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(AxError::BadAddress);
    }

    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();

    if !aspace.can_access_range(start, layout.size(), access_flags) {
        return Err(AxError::BadAddress);
    }

    let page_start = start.align_down_4k();
    let page_end = (start + layout.size()).align_up_4k();
    aspace.populate_area(page_start, page_end - page_start, access_flags)?;

    Ok(())
}

fn check_null_terminated<T: PartialEq + Default>(
    start: VirtAddr,
    access_flags: MappingFlags,
) -> AxResult<usize> {
    let align = Layout::new::<T>().align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(AxError::BadAddress);
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
                let curr = current();
                let aspace = curr.as_thread().proc_data.aspace.lock();
                if !aspace.can_access_range(page, PAGE_SIZE_4K, access_flags) {
                    return Err(AxError::BadAddress);
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

    Ok(len)
}

/// A pointer to user space memory.
#[repr(transparent)]
#[derive(PartialEq, Clone, Copy)]
pub struct UserPtr<T>(*mut T);

impl<T> From<usize> for UserPtr<T> {
    fn from(value: usize) -> Self {
        UserPtr(value as *mut _)
    }
}

impl<T> From<*mut T> for UserPtr<T> {
    fn from(value: *mut T) -> Self {
        UserPtr(value)
    }
}

impl<T> Default for UserPtr<T> {
    fn default() -> Self {
        Self(ptr::null_mut())
    }
}

impl<T> UserPtr<T> {
    const ACCESS_FLAGS: MappingFlags = MappingFlags::READ.union(MappingFlags::WRITE);

    pub fn address(&self) -> VirtAddr {
        VirtAddr::from_ptr_of(self.0)
    }

    pub fn cast<U>(self) -> UserPtr<U> {
        UserPtr(self.0 as *mut U)
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    pub fn get_as_mut(self) -> AxResult<&'static mut T> {
        check_region(self.address(), Layout::new::<T>(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { &mut *self.0 })
    }

    pub fn get_as_mut_slice(self, len: usize) -> AxResult<&'static mut [T]> {
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts_mut(self.0, len) })
    }

    pub fn get_as_mut_null_terminated(self) -> AxResult<&'static mut [T]>
    where
        T: PartialEq + Default,
    {
        let len = check_null_terminated::<T>(self.address(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { slice::from_raw_parts_mut(self.0, len) })
    }
}

/// An immutable pointer to user space memory.
#[repr(transparent)]
#[derive(PartialEq, Clone, Copy)]
pub struct UserConstPtr<T>(*const T);

impl<T> From<usize> for UserConstPtr<T> {
    fn from(value: usize) -> Self {
        UserConstPtr(value as *const _)
    }
}

impl<T> From<*const T> for UserConstPtr<T> {
    fn from(value: *const T) -> Self {
        UserConstPtr(value)
    }
}

impl<T> Default for UserConstPtr<T> {
    fn default() -> Self {
        Self(ptr::null())
    }
}

impl<T> UserConstPtr<T> {
    const ACCESS_FLAGS: MappingFlags = MappingFlags::READ;

    pub fn address(&self) -> VirtAddr {
        VirtAddr::from_ptr_of(self.0)
    }

    pub fn cast<U>(self) -> UserConstPtr<U> {
        UserConstPtr(self.0 as *const U)
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    pub fn get_as_ref(self) -> AxResult<&'static T> {
        check_region(self.address(), Layout::new::<T>(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { &*self.0 })
    }

    pub fn get_as_slice(self, len: usize) -> AxResult<&'static [T]> {
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts(self.0, len) })
    }

    pub fn get_as_null_terminated(self) -> AxResult<&'static [T]>
    where
        T: PartialEq + Default,
    {
        let len = check_null_terminated::<T>(self.address(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { slice::from_raw_parts(self.0, len) })
    }
}

impl UserConstPtr<c_char> {
    /// Get the pointer as `&str`, validating the memory region.
    pub fn get_as_str(self) -> AxResult<&'static str> {
        let slice = self.get_as_null_terminated()?;
        // SAFETY: c_char is u8
        let slice = unsafe { transmute::<&[c_char], &[u8]>(slice) };

        str::from_utf8(slice).map_err(|_| AxError::IllegalBytes)
    }
}

macro_rules! nullable {
    ($ptr:ident.$func:ident($($arg:expr),*)) => {
        if $ptr.is_null() {
            Ok(None)
        } else {
            Some($ptr.$func($($arg),*)).transpose()
        }
    };
}

pub(crate) use nullable;

#[register_trap_handler(PAGE_FAULT)]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags) -> bool {
    debug!("Page fault at {vaddr:#x}, access_flags: {access_flags:#x?}");
    if unlikely(!is_accessing_user_memory()) {
        return false;
    }

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        return false;
    };

    thr.proc_data
        .aspace
        .lock()
        .handle_page_fault(vaddr, access_flags)
}

pub fn vm_load_string(ptr: *const c_char) -> AxResult<String> {
    #[allow(clippy::unnecessary_cast)]
    let bytes = vm_load_until_nul(ptr as *const u8)?;
    String::from_utf8(bytes).map_err(|_| AxError::IllegalBytes)
}

/// A read-only buffer in the VM's memory.
///
/// It implements the `axio::Read` trait, allowing it to be used with other I/O
/// operations.
pub struct VmBytes {
    /// The pointer to the start of the buffer in the VM's memory.
    pub ptr: *const u8,
    /// The length of the buffer.
    pub len: usize,
}

impl VmBytes {
    /// Creates a new `VmBytes` from a raw pointer and a length.
    pub fn new(ptr: *const u8, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Casts the `VmBytes` to a mutable `VmBytesMut`.
    pub fn cast_mut(&self) -> VmBytesMut {
        VmBytesMut::new(self.ptr as *mut u8, self.len)
    }
}

impl Read for VmBytes {
    /// Reads bytes from the VM's memory into the provided buffer.
    fn read(&mut self, buf: &mut [u8]) -> axio::Result<usize> {
        let len = self.len.min(buf.len());
        vm_read_slice(self.ptr, unsafe {
            transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buf[..len])
        })?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }
}

impl Buf for VmBytes {
    fn remaining(&self) -> usize {
        self.len
    }
}

/// A mutable buffer in the VM's memory.
///
/// It implements the `axio::Write` trait, allowing it to be used with other I/O
/// operations.
pub struct VmBytesMut {
    /// The pointer to the start of the buffer in the VM's memory.
    pub ptr: *mut u8,
    /// The length of the buffer.
    pub len: usize,
}

impl VmBytesMut {
    /// Creates a new `VmBytesMut` from a raw pointer and a length.
    pub fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Casts the `VmBytesMut` to a read-only `VmBytes`.
    pub fn cast_const(&self) -> VmBytes {
        VmBytes::new(self.ptr, self.len)
    }
}

impl Write for VmBytesMut {
    /// Writes bytes from the provided buffer into the VM's memory.
    fn write(&mut self, buf: &[u8]) -> axio::Result<usize> {
        let len = self.len.min(buf.len());
        vm_write_slice(self.ptr, &buf[..len])?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }

    /// Flushes the buffer. This is a no-op for `VmBytesMut`.
    fn flush(&mut self) -> axio::Result {
        Ok(())
    }
}

impl BufMut for VmBytesMut {
    fn remaining_mut(&self) -> usize {
        self.len
    }
}
