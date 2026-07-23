use alloc::string::String;
use core::{
    alloc::Layout,
    ffi::c_char,
    hint::{spin_loop, unlikely},
    mem::{MaybeUninit, transmute},
    ptr, slice,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_io::prelude::*;
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::{
    cpu::{asm::user_copy, trap::page_fault_handler},
    paging::MappingFlags,
};
use ax_task::{current, might_sleep};
use extern_trait::extern_trait;
use starry_vm::{VmError, VmIo, VmResult, vm_load_until_nul, vm_read_slice, vm_write_slice};

use crate::{
    config::{USER_SPACE_BASE, USER_SPACE_SIZE},
    task::AsThread,
};

/// Enables scoped access into user memory, allowing page faults to occur inside
/// kernel.
#[track_caller]
pub fn access_user_memory<R>(f: impl FnOnce() -> R) -> R {
    assert!(
        ax_runtime::hal::cpu::asm::irqs_enabled(),
        "faultable user memory access requires IRQs enabled"
    );
    might_sleep();

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        panic!("access_user_memory called outside of thread context");
    };

    thr.set_accessing_user_memory(true);
    let result = f();
    thr.set_accessing_user_memory(false);
    result
}

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> AxResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(AxError::BadAddress);
    }

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        warn!(
            "reject user region check outside thread context: task={}, start={:#x}, len={}",
            curr.id_name(),
            start.as_usize(),
            layout.size()
        );
        return Err(AxError::BadAddress);
    };
    let aspace_arc = thr.proc_data.aspace();
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        return Err(AxError::BadAddress);
    }
    let mut aspace = aspace_arc.lock();

    if !aspace.can_access_range(start, layout.size(), access_flags) {
        return Err(AxError::BadAddress);
    }

    let page_start = start.align_down_4k();
    let page_end = (start + layout.size()).align_up_4k();
    aspace.populate_area(page_start, page_end - page_start, access_flags)?;

    Ok(())
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

    pub fn as_ptr(&self) -> *mut T {
        self.0
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
        if len == 0 {
            return Ok(&mut []);
        }
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts_mut(self.0, len) })
    }
}

pub fn atomic_update_user_u32(
    ptr: *mut u32,
    mut update: impl FnMut(u32) -> AxResult<u32>,
) -> AxResult<u32> {
    check_region(
        VirtAddr::from_ptr_of(ptr),
        Layout::new::<u32>(),
        MappingFlags::READ.union(MappingFlags::WRITE),
    )?;

    let ptr = ptr.cast::<AtomicU32>();
    access_user_memory(|| {
        loop {
            // SAFETY: check_region() validated that the user address is a
            // writable, properly aligned u32 in the current address space.
            let old = unsafe { &*ptr }.load(Ordering::SeqCst);
            let new = update(old)?;
            match unsafe { &*ptr }.compare_exchange_weak(
                old,
                new,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(old),
                Err(_) => spin_loop(),
            }
        }
    })
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
        if len == 0 {
            return Ok(&[]);
        }
        check_region(
            self.address(),
            Layout::array::<T>(len).unwrap(),
            Self::ACCESS_FLAGS,
        )?;
        Ok(unsafe { slice::from_raw_parts(self.0, len) })
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

/// Cumulative count of user page faults dispatched to the demand-paging handler.
///
/// Every fault that reaches the address-space `handle_page_fault` call is counted, matching the
/// Linux `pgfault` event in mm/vmstat.c (all minor + major faults, regardless of resolution).
/// Exposed through `/proc/vmstat` so node_exporter's vmstat collector can surface
/// `node_vmstat_pgfault`.
pub static PAGE_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

#[page_fault_handler]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags) -> bool {
    debug!("Page fault at {vaddr:#x}, access_flags: {access_flags:#x?}");

    #[cfg(feature = "stack-guard-page")]
    if ax_task::diagnose_current_stack_guard_page_fault(vaddr) {
        return false;
    }

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        return false;
    };

    if unlikely(!thr.is_accessing_user_memory()) {
        // Still try to handle kernel-mode faults on user-space addresses.
        // Several syscall sites (e.g. event.rs, net/io.rs, fs/lock.rs) obtain
        // a direct `&mut` reference into user memory via get_as_mut /
        // get_as_mut_slice and write through it outside of
        // access_user_memory().  If a concurrent fork has re-marked the page
        // read-only between check_region() and the write, the kernel write
        // hits a COW #PF with no fixup-table entry and panics.  Handling the
        // fault here lets the standard COW path copy the page just as it
        // would for a user-mode write.
        let user_range = USER_SPACE_BASE..USER_SPACE_BASE + USER_SPACE_SIZE;
        if !user_range.contains(&vaddr.as_usize()) {
            return false;
        }
        // Avoid recursion / deadlock: if this thread already holds the
        // aspace lock (e.g. fault inside aspace.lock().handle_page_fault())
        // we have to bail out instead of trying to lock it again.
        let aspace_arc = thr.proc_data.aspace();
        if unsafe { aspace_arc.raw() }.is_owned_by_current() {
            return false;
        }
    }

    might_sleep();
    let aspace_arc = thr.proc_data.aspace();
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        warn!(
            "user page fault while current thread already owns its address-space lock: \
             vaddr={vaddr:#x}, access_flags={access_flags:#x?}"
        );
        return false;
    }
    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    aspace_arc
        .lock()
        .handle_page_fault(vaddr, access_flags)
        .is_resolved()
}

pub const PATH_MAX: usize = 4096;

pub fn vm_load_string(ptr: *const c_char) -> AxResult<String> {
    #[allow(clippy::unnecessary_cast)]
    let bytes = vm_load_until_nul(ptr as *const u8)?;
    String::from_utf8(bytes).map_err(|_| AxError::IllegalBytes)
}

pub fn vm_load_path_string(ptr: *const c_char) -> AxResult<String> {
    let path = vm_load_string(ptr)?;
    if path.len() >= PATH_MAX {
        return Err(AxError::NameTooLong);
    }
    Ok(path)
}

struct Vm;

/// Briefly checks if the given memory region is valid user memory.
pub fn check_access(start: usize, len: usize) -> VmResult {
    const USER_SPACE_END: usize = USER_SPACE_BASE + USER_SPACE_SIZE;
    let ok = (USER_SPACE_BASE..USER_SPACE_END).contains(&start) && (USER_SPACE_END - start) >= len;
    if unlikely(!ok) {
        Err(VmError::AccessDenied)
    } else {
        Ok(())
    }
}

fn ensure_thread_context(op: &str, start: usize, len: usize) -> VmResult {
    let curr = current();
    if curr.try_as_thread().is_some() {
        Ok(())
    } else {
        warn!(
            "reject user memory {op} outside thread context: task={}, start={start:#x}, len={len}",
            curr.id_name()
        );
        Err(VmError::AccessDenied)
    }
}

fn prepare_user_memory(op: &str, start: usize, len: usize, access_flags: MappingFlags) -> VmResult {
    check_access(start, len)?;
    if len == 0 {
        return Ok(());
    }
    ensure_thread_context(op, start, len)?;

    let start = VirtAddr::from(start);
    let end = start + len;
    let page_start = start.align_down_4k();
    let page_end = end.align_up_4k();

    let curr = current();
    let thr = curr.try_as_thread().ok_or(VmError::AccessDenied)?;
    let aspace_arc = thr.proc_data.aspace();
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        return Err(VmError::AccessDenied);
    }

    let mut aspace = aspace_arc.lock();
    if !aspace.can_access_range(start, len, access_flags) {
        return Err(VmError::AccessDenied);
    }

    aspace
        .populate_area(page_start, page_end - page_start, access_flags)
        .map_err(|_| VmError::AccessDenied)
}

#[extern_trait]
unsafe impl VmIo for Vm {
    fn new() -> Self {
        Self
    }

    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        if buf.is_empty() {
            return Ok(());
        }
        prepare_user_memory("read", start, buf.len(), MappingFlags::READ)?;
        let failed_at = access_user_memory(|| unsafe {
            user_copy(buf.as_mut_ptr() as *mut _, start as _, buf.len())
        });
        if unlikely(failed_at != 0) {
            Err(VmError::AccessDenied)
        } else {
            Ok(())
        }
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        if buf.is_empty() {
            return Ok(());
        }
        prepare_user_memory("write", start, buf.len(), MappingFlags::WRITE)?;
        let failed_at = access_user_memory(|| unsafe {
            user_copy(start as _, buf.as_ptr() as *const _, buf.len())
        });
        if unlikely(failed_at != 0) {
            Err(VmError::AccessDenied)
        } else {
            Ok(())
        }
    }
}

/// A read-only buffer in the VM's memory.
///
/// It implements the `ax_io::Read` trait, allowing it to be used with other I/O
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
}

impl Read for VmBytes {
    /// Reads bytes from the VM's memory into the provided buffer.
    fn read(&mut self, buf: &mut [u8]) -> ax_io::Result<usize> {
        let len = self.len.min(buf.len());
        vm_read_slice(self.ptr, unsafe {
            transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buf[..len])
        })?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }
}

impl IoBuf for VmBytes {
    fn remaining(&self) -> usize {
        self.len
    }
}

/// A mutable buffer in the VM's memory.
///
/// It implements the `ax_io::Write` trait, allowing it to be used with other I/O
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
}

impl Write for VmBytesMut {
    /// Writes bytes from the provided buffer into the VM's memory.
    fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
        let len = self.len.min(buf.len());
        vm_write_slice(self.ptr, &buf[..len])?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }

    /// Flushes the buffer. This is a no-op for `VmBytesMut`.
    fn flush(&mut self) -> ax_io::Result {
        Ok(())
    }
}

impl IoBufMut for VmBytesMut {
    fn remaining_mut(&self) -> usize {
        self.len
    }
}

/// Patches kernel text, ensuring page permissions and instruction-cache
/// synchronization are handled consistently.
pub fn patch_kernel_text<F>(addr: VirtAddr, len: usize, action: F) -> AxResult<()>
where
    F: FnOnce(*mut u8),
{
    if len == 0 {
        return Ok(());
    }

    let aligned_addr = addr.align_down_4k();
    let aligned_length = (addr + len).align_up_4k() - aligned_addr;

    // The kernel address-space lock (`SpinNoIrq`) MUST be acquired *inside* the
    // `stop_machine` critical section, not before it. `stop_machine` itself
    // takes a `SpinNoIrq` (`STOP_MACHINE_LOCK`); acquiring `kernel_aspace`
    // first and then dropping it inside the closure produces a non-LIFO nesting
    // of two IRQ-saving guards, which crosses their saved IRQ states and leaks
    // an IRQ-disabled state out of this function. That stranded state later
    // trips the atomic-context guard (e.g. `clear_proc_shm` on process exit
    // right after a static-key `disable_key`). Nesting it LIFO here keeps the
    // IRQ flag balanced — this mirrors the kprobe `set_writeable_for_address`
    // path.
    crate::stop_machine::stop_machine(
        move || -> AxResult<()> {
            let mut guard = ax_mm::kernel_aspace().lock();
            if guard.contains_range(aligned_addr, aligned_length) {
                let (_, original_flags, _) = guard.page_table().query(aligned_addr)?;

                guard.protect(
                    aligned_addr,
                    aligned_length,
                    original_flags | MappingFlags::WRITE,
                )?;

                flush_tlb_range(aligned_addr, aligned_length);
                action(addr.as_mut_ptr());

                ax_runtime::hal::cache::clean_dcache_to_pou(addr, len);

                guard.protect(aligned_addr, aligned_length, original_flags)?;
                return Ok(());
            }

            #[cfg(target_arch = "loongarch64")]
            {
                // LoongArch64 kernel text may execute from the 0x9000... DMW
                // direct-map window. DMW translations do not consult PTEs, so
                // there are no page permissions to relax here. Patch directly
                // while all other CPUs are parked, then rely on the per-CPU
                // sync callback to flush instruction state.
                action(addr.as_mut_ptr());
                return Ok(());
            }

            #[cfg(not(target_arch = "loongarch64"))]
            {
                Err(AxError::BadAddress)
            }
        },
        move || sync_modified_kernel_text(aligned_addr, aligned_length),
    )
}

/// Writes data to kernel text, ensuring the page permissions are properly handled.
pub fn write_kernel_text(addr: VirtAddr, data: &[u8]) -> AxResult<()> {
    patch_kernel_text(addr, data.len(), |dst| unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
    })
}

pub fn flush_tlb_range(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::flush_tlb_range(start, size);
}

pub fn flush_tlb_range_sync(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::flush_tlb_range_all_cpus(start, size);
}

fn sync_modified_kernel_text(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::sync_kernel_text(start, size);
}
