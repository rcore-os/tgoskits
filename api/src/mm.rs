use alloc::string::String;
use core::{alloc::Layout, ffi::c_char, mem::transmute, ptr, slice, str};

use axerrno::{LinuxError, LinuxResult};
use axhal::{
    paging::MappingFlags,
    trap::{PAGE_FAULT, register_trap_handler},
};
use axtask::current;
use memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use starry_core::task::{AsThread, ProcessData, send_signal_to_process};
use starry_signal::{SignalInfo, Signo};
use starry_vm::vm_load_c_string;

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> LinuxResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(LinuxError::EFAULT);
    }

    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();

    if !aspace.can_access_range(start, layout.size(), access_flags) {
        return Err(LinuxError::EFAULT);
    }

    let page_start = start.align_down_4k();
    let page_end = (start + layout.size()).align_up_4k();
    aspace.populate_area(page_start, page_end - page_start, access_flags)?;

    Ok(())
}

fn check_null_terminated<T: PartialEq + Default>(
    start: VirtAddr,
    access_flags: MappingFlags,
) -> LinuxResult<usize> {
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
                let curr = current();
                let aspace = curr.as_thread().proc_data.aspace.lock();
                if !aspace.can_access_range(page, PAGE_SIZE_4K, access_flags) {
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

    pub fn get_as_mut(self) -> LinuxResult<&'static mut T> {
        check_region(self.address(), Layout::new::<T>(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { &mut *self.0 })
    }

    pub fn get_as_mut_slice(self, len: usize) -> LinuxResult<&'static mut [T]> {
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts_mut(self.0, len) })
    }

    pub fn get_as_mut_null_terminated(self) -> LinuxResult<&'static mut [T]>
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

    pub fn get_as_ref(self) -> LinuxResult<&'static T> {
        check_region(self.address(), Layout::new::<T>(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { &*self.0 })
    }

    pub fn get_as_slice(self, len: usize) -> LinuxResult<&'static [T]> {
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts(self.0, len) })
    }

    pub fn get_as_null_terminated(self) -> LinuxResult<&'static [T]>
    where
        T: PartialEq + Default,
    {
        let len = check_null_terminated::<T>(self.address(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { slice::from_raw_parts(self.0, len) })
    }
}

impl UserConstPtr<c_char> {
    /// Get the pointer as `&str`, validating the memory region.
    pub fn get_as_str(self) -> LinuxResult<&'static str> {
        let slice = self.get_as_null_terminated()?;
        // SAFETY: c_char is u8
        let slice = unsafe { transmute::<&[c_char], &[u8]>(slice) };

        str::from_utf8(slice).map_err(|_| LinuxError::EILSEQ)
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

#[percpu::def_percpu]
static mut ACCESSING_USER_MEM: bool = false;

/// Enables scoped access into user memory, allowing page faults to occur inside
/// kernel.
pub fn access_user_memory<R>(f: impl FnOnce() -> R) -> R {
    ACCESSING_USER_MEM.with_current(|v| {
        unsafe {
            core::ptr::write_volatile(v, true);
        }
        let result = f();
        unsafe {
            core::ptr::write_volatile(v, false);
        }
        result
    })
}

/// Check if the current thread is accessing user memory.
pub fn is_accessing_user_memory() -> bool {
    ACCESSING_USER_MEM.read_current()
}

pub fn handle_user_page_fault(
    proc_data: &ProcessData,
    vaddr: VirtAddr,
    access_flags: MappingFlags,
) {
    if !proc_data
        .aspace
        .lock()
        .handle_page_fault(vaddr, access_flags)
    {
        info!(
            "{:?}: segmentation fault at {:#x} {:?}",
            proc_data.proc, vaddr, access_flags
        );
        send_signal_to_process(
            proc_data.proc.pid(),
            Some(SignalInfo::new_kernel(Signo::SIGSEGV)),
        )
        .expect("Failed to send SIGSEGV");
    }
}

#[register_trap_handler(PAGE_FAULT)]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags) -> bool {
    debug!(
        "Page fault at {:#x}, access_flags: {:#x?}",
        vaddr, access_flags
    );
    if !is_accessing_user_memory() {
        return false;
    }

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        return false;
    };

    handle_user_page_fault(&thr.proc_data, vaddr, access_flags);

    true
}

pub fn vm_load_string(ptr: *const c_char) -> LinuxResult<String> {
    let bytes = vm_load_c_string(ptr.cast())?;
    bytes.into_string().map_err(|_| LinuxError::EILSEQ)
}
