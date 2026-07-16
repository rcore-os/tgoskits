use alloc::{rc::Rc, string::String, vec::Vec};
use core::{
    alloc::Layout,
    ffi::c_char,
    hint::{spin_loop, unlikely},
    marker::PhantomData,
    mem::{MaybeUninit, size_of, transmute},
    ptr,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_io::prelude::*;
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::{
    cpu::{asm::user_copy, trap::page_fault_handler},
    paging::MappingFlags,
};
use bytemuck::{AnyBitPattern, NoUninit};
use extern_trait::extern_trait;
use starry_vm::{
    VmError, VmIo, VmMutPtr, VmPtr, VmResult, vm_load, vm_load_any, vm_load_until_nul,
    vm_read_slice, vm_write_slice,
};

use crate::{
    config::{USER_SPACE_BASE, USER_SPACE_SIZE},
    task::{UserTaskRef, current_user_task, might_sleep, try_current_user_task},
};

/// Enables scoped access into user memory, allowing page faults to occur inside
/// kernel.
#[track_caller]
fn access_user_memory<R>(f: impl FnOnce() -> R) -> VmResult<R> {
    if ax_runtime::hal::irq::in_irq_context() {
        return Err(VmError::AccessDenied);
    }
    assert!(
        ax_runtime::hal::cpu::asm::irqs_enabled(),
        "faultable user memory access requires IRQs enabled"
    );
    might_sleep();

    let curr = current_user_task();
    let _scope = UserAccessScope::enter(&curr);
    Ok(f())
}

/// One nestable, task-bound scope in which kernel user-memory faults may be repaired.
///
/// The scope is deliberately `!Send`: the depth belongs to the current Starry
/// thread and must be removed by the same scheduler thread that installed it.
struct UserAccessScope<'thread> {
    thread: &'thread crate::task::Thread,
    _not_send: PhantomData<Rc<()>>,
}

impl<'thread> UserAccessScope<'thread> {
    fn enter(task: &'thread UserTaskRef) -> Self {
        let thread = task.as_thread();
        thread.enter_user_memory_access();
        Self {
            thread,
            _not_send: PhantomData,
        }
    }
}

impl Drop for UserAccessScope<'_> {
    fn drop(&mut self) {
        self.thread.leave_user_memory_access();
    }
}

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> AxResult<()> {
    if ax_runtime::hal::irq::in_irq_context() {
        return Err(AxError::BadAddress);
    }
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(AxError::BadAddress);
    }

    let curr = try_current_user_task()
        .map_err(|_| AxError::BadAddress)?
        .ok_or(AxError::BadAddress)?;
    let thr = curr.as_thread();
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
pub struct UserPtr<T>(*mut T);

impl<T> Copy for UserPtr<T> {}

impl<T> Clone for UserPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for UserPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T> Eq for UserPtr<T> {}

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

    /// Copies one initialized value from user memory.
    pub fn read(self) -> AxResult<T>
    where
        T: AnyBitPattern,
    {
        self.0.vm_read().map_err(Into::into)
    }

    /// Copies one ABI value whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    pub unsafe fn read_abi(self) -> AxResult<T> {
        let value = self.0.vm_read_uninit()?;
        // SAFETY: guaranteed by the caller after the copy initialized every byte.
        Ok(unsafe { value.assume_init() })
    }

    /// Copies ABI values whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    pub unsafe fn read_abi_slice(self, len: usize) -> AxResult<Vec<T>> {
        // SAFETY: the caller supplies the element validity contract.
        unsafe { vm_load_any(self.0.cast_const(), len) }.map_err(Into::into)
    }

    /// Copies one kernel-owned value to user memory.
    pub fn write(self, value: T) -> AxResult<()>
    where
        T: NoUninit,
    {
        self.0.vm_write(value).map_err(Into::into)
    }

    /// Copies one initialized field without exposing or copying the containing
    /// ABI object's padding bytes.
    pub fn write_field<U>(self, offset: usize, value: U) -> AxResult<()>
    where
        U: NoUninit,
    {
        let field_end = offset
            .checked_add(size_of::<U>())
            .filter(|end| *end <= size_of::<T>())
            .ok_or(AxError::BadAddress)?;
        debug_assert!(field_end <= size_of::<T>());
        let field_address = self
            .0
            .addr()
            .checked_add(offset)
            .ok_or(AxError::BadAddress)?;
        UserPtr::<U>::from(field_address).write(value)
    }

    /// Copies an initialized array field without requiring the containing
    /// array length to implement [`NoUninit`].
    pub fn write_field_slice<U>(self, offset: usize, values: &[U]) -> AxResult<()>
    where
        U: NoUninit,
    {
        let byte_len = size_of::<U>()
            .checked_mul(values.len())
            .ok_or(AxError::BadAddress)?;
        offset
            .checked_add(byte_len)
            .filter(|end| *end <= size_of::<T>())
            .ok_or(AxError::BadAddress)?;
        let field_address = self
            .0
            .addr()
            .checked_add(offset)
            .ok_or(AxError::BadAddress)?;
        UserPtr::<U>::from(field_address).write_slice(values)
    }

    /// Copies kernel-owned values to user memory.
    pub fn write_slice(self, values: &[T]) -> AxResult<()>
    where
        T: NoUninit,
    {
        vm_write_slice(self.0, values).map_err(Into::into)
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
    .map_err(AxError::from)?
}

/// An immutable pointer to user space memory.
#[repr(transparent)]
pub struct UserConstPtr<T>(*const T);

impl<T> Copy for UserConstPtr<T> {}

impl<T> Clone for UserConstPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for UserConstPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T> Eq for UserConstPtr<T> {}

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
    pub fn address(&self) -> VirtAddr {
        VirtAddr::from_ptr_of(self.0)
    }

    pub fn cast<U>(self) -> UserConstPtr<U> {
        UserConstPtr(self.0 as *const U)
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// Copies one initialized value from user memory.
    pub fn read(self) -> AxResult<T>
    where
        T: AnyBitPattern,
    {
        self.0.vm_read().map_err(Into::into)
    }

    /// Copies one ABI value whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    pub unsafe fn read_abi(self) -> AxResult<T> {
        let value = self.0.vm_read_uninit()?;
        // SAFETY: guaranteed by the caller after the copy initialized every byte.
        Ok(unsafe { value.assume_init() })
    }

    /// Copies initialized values from user memory into kernel-owned storage.
    pub fn read_slice(self, len: usize) -> AxResult<Vec<T>>
    where
        T: AnyBitPattern,
    {
        vm_load(self.0, len).map_err(Into::into)
    }

    /// Validates and prefaults a readable user range without exposing it as a reference.
    pub fn validate_slice(self, len: usize) -> AxResult<()> {
        if len == 0 {
            return Ok(());
        }
        let byte_len = size_of::<T>()
            .checked_mul(len)
            .ok_or(AxError::InvalidInput)?;
        prepare_user_memory("validate read", self.0.addr(), byte_len, MappingFlags::READ)
            .map_err(Into::into)
    }
}

/// Cumulative count of user page faults dispatched to the demand-paging handler.
///
/// Every fault that reaches the address-space `handle_page_fault` call is counted, matching the
/// Linux `pgfault` event in mm/vmstat.c (all minor + major faults, regardless of resolution).
/// Exposed through `/proc/vmstat` so node_exporter's vmstat collector can surface
/// `node_vmstat_pgfault`.
pub static PAGE_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

/// Fixed, allocation-free diagnostic for malformed or reentrant task identity lookups.
static PAGE_FAULT_IDENTITY_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Fixed, allocation-free diagnostic for malformed task identity during user-copy setup.
static USER_MEMORY_IDENTITY_FAILURES: AtomicU64 = AtomicU64::new(0);

#[page_fault_handler]
fn handle_page_fault(vaddr: VirtAddr, access_flags: MappingFlags) -> bool {
    #[cfg(feature = "stack-guard-page")]
    if ax_runtime::task::diagnose_current_stack_guard_page_fault(vaddr) {
        return false;
    }

    // This callback handles only faults caused by a user mapping or by the
    // kernel explicitly touching one. Reject unrelated kernel addresses before
    // consulting Starry task identity or entering any sleepable MM path.
    let user_range = USER_SPACE_BASE..USER_SPACE_BASE + USER_SPACE_SIZE;
    if !user_range.contains(&vaddr.as_usize()) {
        return false;
    }

    // The interrupted task may own a user-copy scope, but an IRQ handler is
    // not part of that copy. Linux keys uaccess recovery to the faulting
    // instruction; reject IRQ-context faults here so the task-scoped fallback
    // cannot turn an unrelated hard-IRQ bug into a sleeping MM operation.
    if ax_runtime::hal::irq::in_irq_context() {
        return false;
    }

    let curr = match resolve_page_fault_user_task(try_current_user_task()) {
        Ok(Some(task)) => task,
        Ok(None) => return false,
        Err(_error) => {
            PAGE_FAULT_IDENTITY_FAILURES.fetch_add(1, Ordering::Relaxed);
            return false;
        }
    };
    let thr = curr.as_thread();

    if !thr.has_active_user_memory_access() {
        return false;
    }

    might_sleep();
    let aspace_arc = thr.proc_data.aspace();
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        return false;
    }
    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    aspace_arc.lock().handle_page_fault(vaddr, access_flags)
}

fn resolve_page_fault_user_task(
    lookup: Result<Option<UserTaskRef>, ax_std::os::arceos::task::TaskError>,
) -> Result<Option<UserTaskRef>, ax_std::os::arceos::task::TaskError> {
    match lookup {
        Ok(task) => Ok(task),
        Err(
            ax_std::os::arceos::task::TaskError::NotInitialized
            | ax_std::os::arceos::task::TaskError::NoRunnableThread
            | ax_std::os::arceos::task::TaskError::CpuOwnerBorrowed,
        ) => Ok(None),
        Err(error) => Err(error),
    }
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

fn user_task_for_memory_access(op: &str, start: usize, len: usize) -> VmResult<UserTaskRef> {
    match try_current_user_task() {
        Ok(Some(task)) => Ok(task),
        Ok(None) => {
            warn!("reject user memory {op} outside user-task context: start={start:#x}, len={len}");
            Err(VmError::AccessDenied)
        }
        Err(_error) => {
            USER_MEMORY_IDENTITY_FAILURES.fetch_add(1, Ordering::Relaxed);
            Err(VmError::AccessDenied)
        }
    }
}

fn prepare_user_memory(op: &str, start: usize, len: usize, access_flags: MappingFlags) -> VmResult {
    if ax_runtime::hal::irq::in_irq_context() {
        return Err(VmError::AccessDenied);
    }
    check_access(start, len)?;
    if len == 0 {
        return Ok(());
    }
    let curr = user_task_for_memory_access(op, start, len)?;

    let start = VirtAddr::from(start);
    let end = start + len;
    let page_start = start.align_down_4k();
    let page_end = end.align_up_4k();

    let thr = curr.as_thread();
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
        })?;
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
        })?;
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

#[cfg(test)]
mod tests {
    use ax_std::os::arceos::task::TaskError;

    use super::*;

    #[test]
    fn bootstrap_page_fault_has_no_starry_memory_owner() {
        assert!(matches!(resolve_page_fault_user_task(Ok(None)), Ok(None)));
        assert!(matches!(
            resolve_page_fault_user_task(Err(TaskError::NotInitialized)),
            Ok(None)
        ));
        assert!(matches!(
            resolve_page_fault_user_task(Err(TaskError::CpuOwnerBorrowed)),
            Ok(None)
        ));
    }

    #[test]
    fn malformed_user_extension_is_reported_to_the_fatal_trap_path() {
        assert!(matches!(
            resolve_page_fault_user_task(Err(TaskError::InvalidRuntimeHandle)),
            Err(TaskError::InvalidRuntimeHandle)
        ));
    }
}
