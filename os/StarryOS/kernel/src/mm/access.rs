use alloc::{string::String, vec::Vec};
use core::{
    ffi::c_char,
    hint::unlikely,
    mem::{MaybeUninit, size_of, transmute},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_io::prelude::*;
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::{
    cpu::{
        UserAccessError, UserAtomicError, UserAtomicU32Op,
        asm::user_copy,
        trap::{PageFaultFlags, page_fault_handler},
        user_atomic_u32, user_read_u32,
    },
    paging::MappingFlags,
};
use bytemuck::{AnyBitPattern, NoUninit};
use starry_vm::{VmError, VmIo, VmResult};

use super::io::vm_error_to_io_error;
use crate::{
    StarryError, StarryResult,
    config::{USER_SPACE_BASE, USER_SPACE_SIZE},
    task::{UserTaskRef, might_sleep, try_current_user_task},
};

/// Enables scoped access into user memory, allowing page faults to occur inside
/// kernel.
#[track_caller]
fn access_user_memory<R>(task: &UserTaskRef, f: impl FnOnce() -> R) -> VmResult<R> {
    if ax_runtime::hal::irq::in_irq_context() {
        return Err(VmError::AccessDenied);
    }
    assert!(
        ax_runtime::hal::cpu::asm::irqs_enabled(),
        "faultable user memory access requires IRQs enabled"
    );
    might_sleep();

    let _scope = task.as_thread().enter_user_memory_access();
    Ok(f())
}

/// Reads from a virtual pointer through an explicit Starry task capability.
pub trait VmPtr: starry_vm::VmPtr {
    /// Returns `None` for a null user pointer.
    fn nullable(self) -> Option<Self> {
        if starry_vm::VmPtr::as_ptr(self).is_null() {
            None
        } else {
            Some(self)
        }
    }

    /// Copies one value without assuming that every user byte pattern is valid.
    fn vm_read_uninit(self, task: &UserTaskRef) -> VmResult<MaybeUninit<Self::Target>> {
        let mut vm = UserMemoryProvider::new(task);
        starry_vm::VmPtr::vm_read_uninit(self, &mut vm)
    }

    /// Copies one value whose type accepts every initialized byte pattern.
    fn vm_read(self, task: &UserTaskRef) -> VmResult<Self::Target>
    where
        Self::Target: AnyBitPattern,
    {
        let mut vm = UserMemoryProvider::new(task);
        starry_vm::VmPtr::vm_read(self, &mut vm)
    }
}

impl<P: starry_vm::VmPtr> VmPtr for P {}

/// Writes to a virtual pointer through an explicit Starry task capability.
pub trait VmMutPtr: VmPtr + starry_vm::VmMutPtr {
    /// Copies one fully initialized kernel value into user memory.
    fn vm_write(self, task: &UserTaskRef, value: Self::Target) -> VmResult
    where
        Self::Target: NoUninit,
    {
        let mut vm = UserMemoryProvider::new(task);
        starry_vm::VmMutPtr::vm_write(self, &mut vm, value)
    }
}

impl<P: starry_vm::VmMutPtr> VmMutPtr for P {}

/// Copies initialized user bytes into kernel-owned storage.
pub fn vm_read_slice<T>(task: &UserTaskRef, ptr: *const T, buf: &mut [MaybeUninit<T>]) -> VmResult {
    starry_vm::vm_read_slice(&mut UserMemoryProvider::new(task), ptr, buf)
}

/// Copies initialized kernel bytes into user memory.
pub fn vm_write_slice<T: NoUninit>(task: &UserTaskRef, ptr: *mut T, buf: &[T]) -> VmResult {
    starry_vm::vm_write_slice(&mut UserMemoryProvider::new(task), ptr, buf)
}

/// Loads an initialized vector from user memory.
pub fn vm_load<T: AnyBitPattern>(
    task: &UserTaskRef,
    ptr: *const T,
    len: usize,
) -> VmResult<Vec<T>> {
    starry_vm::vm_load(&mut UserMemoryProvider::new(task), ptr, len)
}

/// Loads values whose validity is guaranteed by the caller.
///
/// # Safety
///
/// Every copied user byte pattern must be a valid initialized `T`.
pub unsafe fn vm_load_any<T>(task: &UserTaskRef, ptr: *const T, len: usize) -> VmResult<Vec<T>> {
    unsafe { starry_vm::vm_load_any(&mut UserMemoryProvider::new(task), ptr, len) }
}

/// Loads a zero-terminated sequence from user memory.
pub fn vm_load_until_nul<T: bytemuck::Pod>(task: &UserTaskRef, ptr: *const T) -> VmResult<Vec<T>> {
    starry_vm::vm_load_until_nul(&mut UserMemoryProvider::new(task), ptr)
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
    pub fn read(self, task: &UserTaskRef) -> crate::StarryResult<T>
    where
        T: AnyBitPattern,
    {
        self.0.vm_read(task).map_err(Into::into)
    }

    /// Copies one ABI value whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    pub unsafe fn read_abi(self, task: &UserTaskRef) -> crate::StarryResult<T> {
        let value = self.0.vm_read_uninit(task)?;
        // SAFETY: guaranteed by the caller after the copy initialized every byte.
        Ok(unsafe { value.assume_init() })
    }

    /// Copies ABI values whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    pub unsafe fn read_abi_slice(
        self,
        task: &UserTaskRef,
        len: usize,
    ) -> crate::StarryResult<Vec<T>> {
        // SAFETY: the caller supplies the element validity contract.
        unsafe { vm_load_any(task, self.0.cast_const(), len) }.map_err(Into::into)
    }

    /// Copies one kernel-owned value to user memory.
    pub fn write(self, task: &UserTaskRef, value: T) -> crate::StarryResult<()>
    where
        T: NoUninit,
    {
        self.0.vm_write(task, value).map_err(Into::into)
    }

    /// Copies one initialized field without exposing or copying the containing
    /// ABI object's padding bytes.
    pub fn write_field<U>(
        self,
        task: &UserTaskRef,
        offset: usize,
        value: U,
    ) -> crate::StarryResult<()>
    where
        U: NoUninit,
    {
        let field_end = offset
            .checked_add(size_of::<U>())
            .filter(|end| *end <= size_of::<T>())
            .ok_or(crate::StarryError::BadAddress)?;
        debug_assert!(field_end <= size_of::<T>());
        let field_address = self
            .0
            .addr()
            .checked_add(offset)
            .ok_or(crate::StarryError::BadAddress)?;
        UserPtr::<U>::from(field_address).write(task, value)
    }

    /// Copies an initialized array field without requiring the containing
    /// array length to implement [`NoUninit`].
    pub fn write_field_slice<U>(
        self,
        task: &UserTaskRef,
        offset: usize,
        values: &[U],
    ) -> crate::StarryResult<()>
    where
        U: NoUninit,
    {
        let byte_len = size_of::<U>()
            .checked_mul(values.len())
            .ok_or(crate::StarryError::BadAddress)?;
        offset
            .checked_add(byte_len)
            .filter(|end| *end <= size_of::<T>())
            .ok_or(crate::StarryError::BadAddress)?;
        let field_address = self
            .0
            .addr()
            .checked_add(offset)
            .ok_or(crate::StarryError::BadAddress)?;
        UserPtr::<U>::from(field_address).write_slice(task, values)
    }

    /// Copies kernel-owned values to user memory.
    pub fn write_slice(self, task: &UserTaskRef, values: &[T]) -> crate::StarryResult<()>
    where
        T: NoUninit,
    {
        vm_write_slice(task, self.0, values).map_err(Into::into)
    }
}

pub fn atomic_update_user_u32_nofault(
    ptr: *mut u32,
    operation: UserAtomicU32Op,
    argument: u32,
) -> Result<u32, UserAtomicError> {
    if ax_runtime::hal::irq::in_irq_context()
        || !ptr.addr().is_multiple_of(size_of::<u32>())
        || check_access(ptr.addr(), size_of::<u32>()).is_err()
    {
        return Err(UserAtomicError::Fault);
    }

    // SAFETY: the range and alignment checks above establish the architecture
    // contract. The dedicated nofault exception table converts a concurrent
    // unmap or protection change into `UserAtomicError::Fault`.
    unsafe { user_atomic_u32(ptr, operation, argument) }
}

pub fn read_user_u32_nofault(ptr: *const u32) -> Result<u32, UserAccessError> {
    if ax_runtime::hal::irq::in_irq_context()
        || !ptr.addr().is_multiple_of(size_of::<u32>())
        || check_access(ptr.addr(), size_of::<u32>()).is_err()
    {
        return Err(UserAccessError::Fault);
    }

    // SAFETY: the range and alignment checks above establish the architecture
    // contract. Concurrent mapping changes are recovered by the dedicated
    // nofault exception table.
    unsafe { user_read_u32(ptr) }
}

/// Resolves and validates a readable futex word outside futex bucket locks.
pub fn fault_in_user_u32_read(task: &UserTaskRef, ptr: *const u32) -> crate::StarryResult<()> {
    fault_in_user_u32(task, ptr.addr(), MappingFlags::READ)
}

/// Resolves and validates a writable futex word outside futex bucket locks.
pub fn fault_in_user_u32_write(task: &UserTaskRef, ptr: *mut u32) -> crate::StarryResult<()> {
    fault_in_user_u32(
        task,
        ptr.addr(),
        MappingFlags::READ.union(MappingFlags::WRITE),
    )
}

fn fault_in_user_u32(
    task: &UserTaskRef,
    address: usize,
    access: MappingFlags,
) -> crate::StarryResult<()> {
    if !address.is_multiple_of(size_of::<u32>()) {
        return Err(crate::StarryError::BadAddress);
    }
    prepare_user_memory(
        task,
        "fault in futex word",
        address,
        size_of::<u32>(),
        access,
    )
    .map_err(Into::into)
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

    pub fn as_ptr(&self) -> *const T {
        self.0
    }

    pub fn cast<U>(self) -> UserConstPtr<U> {
        UserConstPtr(self.0 as *const U)
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// Copies one initialized value from user memory.
    pub fn read(self, task: &UserTaskRef) -> crate::StarryResult<T>
    where
        T: AnyBitPattern,
    {
        self.0.vm_read(task).map_err(Into::into)
    }

    /// Copies one ABI value whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    pub unsafe fn read_abi(self, task: &UserTaskRef) -> crate::StarryResult<T> {
        let value = self.0.vm_read_uninit(task)?;
        // SAFETY: guaranteed by the caller after the copy initialized every byte.
        Ok(unsafe { value.assume_init() })
    }

    /// Copies ABI values whose valid-bit-pattern contract is caller-provided.
    ///
    /// # Safety
    ///
    /// Every possible byte pattern supplied by userspace must be a valid `T`.
    #[cfg(feature = "jpeg")]
    pub unsafe fn read_abi_slice(
        self,
        task: &UserTaskRef,
        len: usize,
    ) -> crate::StarryResult<Vec<T>> {
        // SAFETY: the caller supplies the element validity contract.
        unsafe { vm_load_any(task, self.0, len) }.map_err(Into::into)
    }

    /// Copies initialized values from user memory into kernel-owned storage.
    pub fn read_slice(self, task: &UserTaskRef, len: usize) -> crate::StarryResult<Vec<T>>
    where
        T: AnyBitPattern,
    {
        vm_load(task, self.0, len).map_err(Into::into)
    }

    /// Validates and prefaults a readable user range without exposing it as a reference.
    pub fn validate_slice(self, task: &UserTaskRef, len: usize) -> crate::StarryResult<()> {
        if len == 0 {
            return Ok(());
        }
        let byte_len = size_of::<T>()
            .checked_mul(len)
            .ok_or(crate::StarryError::InvalidInput)?;
        prepare_user_memory(
            task,
            "validate read",
            self.0.addr(),
            byte_len,
            MappingFlags::READ,
        )
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

#[cfg(feature = "axtest")]
const _: fn(&UserTaskRef, &str, usize, usize, MappingFlags) -> VmResult = prepare_user_memory;

#[page_fault_handler]
fn handle_page_fault(vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
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

pub fn vm_load_string(task: &UserTaskRef, ptr: *const c_char) -> crate::StarryResult<String> {
    #[allow(clippy::unnecessary_cast)]
    let bytes = vm_load_until_nul(task, ptr as *const u8)?;
    String::from_utf8(bytes).map_err(|_| crate::StarryError::IllegalBytes)
}

pub fn vm_load_path_string(task: &UserTaskRef, ptr: *const c_char) -> crate::StarryResult<String> {
    let path = vm_load_string(task, ptr)?;
    if path.len() >= PATH_MAX {
        return Err(StarryError::NameTooLong);
    }
    Ok(path)
}

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

fn prepare_user_memory(
    task: &UserTaskRef,
    op: &str,
    start: usize,
    len: usize,
    access_flags: MappingFlags,
) -> VmResult {
    if ax_runtime::hal::irq::in_irq_context() {
        return Err(VmError::AccessDenied);
    }
    check_access(start, len)?;
    debug_assert_ne!(len, 0, "empty user-memory ranges require no preparation");

    let start = VirtAddr::from(start);
    let end = start + len;
    let page_start = start.align_down_4k();
    let page_end = end.align_up_4k();

    let thr = task.as_thread();
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
        .map_err(|_| VmError::AccessDenied)?;
    drop(aspace);
    let _ = op;
    Ok(())
}

/// Task-bound provider passed to capability-oriented VM helper crates.
pub(crate) struct UserMemoryProvider<'task> {
    task: &'task UserTaskRef,
}

impl<'task> UserMemoryProvider<'task> {
    /// Binds user-memory access to a live Starry task reference.
    pub(crate) const fn new(task: &'task UserTaskRef) -> Self {
        Self { task }
    }
}

unsafe impl VmIo for UserMemoryProvider<'_> {
    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        if buf.is_empty() {
            return Ok(());
        }
        prepare_user_memory(self.task, "read", start, buf.len(), MappingFlags::READ)?;
        let failed_at = access_user_memory(self.task, || unsafe {
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
        prepare_user_memory(self.task, "write", start, buf.len(), MappingFlags::WRITE)?;
        let failed_at = access_user_memory(self.task, || unsafe {
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
pub struct VmBytes<'task> {
    task: &'task UserTaskRef,
    /// The pointer to the start of the buffer in the VM's memory.
    pub ptr: *const u8,
    /// The length of the buffer.
    pub len: usize,
}

impl<'task> VmBytes<'task> {
    /// Creates a new `VmBytes` from a raw pointer and a length.
    pub fn new(task: &'task UserTaskRef, ptr: *const u8, len: usize) -> Self {
        Self { task, ptr, len }
    }
}

impl Read for VmBytes<'_> {
    /// Reads bytes from the VM's memory into the provided buffer.
    fn read(&mut self, buf: &mut [u8]) -> ax_io::Result<usize> {
        let len = self.len.min(buf.len());
        vm_read_slice(self.task, self.ptr, unsafe {
            transmute::<&mut [u8], &mut [MaybeUninit<u8>]>(&mut buf[..len])
        })
        .map_err(vm_error_to_io_error)?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }
}

impl IoBuf for VmBytes<'_> {
    fn remaining(&self) -> usize {
        self.len
    }
}

/// A mutable buffer in the VM's memory.
///
/// It implements the `ax_io::Write` trait, allowing it to be used with other I/O
/// operations.
pub struct VmBytesMut<'task> {
    task: &'task UserTaskRef,
    /// The pointer to the start of the buffer in the VM's memory.
    pub ptr: *mut u8,
    /// The length of the buffer.
    pub len: usize,
}

impl<'task> VmBytesMut<'task> {
    /// Creates a new `VmBytesMut` from a raw pointer and a length.
    pub fn new(task: &'task UserTaskRef, ptr: *mut u8, len: usize) -> Self {
        Self { task, ptr, len }
    }
}

impl Write for VmBytesMut<'_> {
    /// Writes bytes from the provided buffer into the VM's memory.
    fn write(&mut self, buf: &[u8]) -> ax_io::Result<usize> {
        let len = self.len.min(buf.len());
        vm_write_slice(self.task, self.ptr, &buf[..len]).map_err(vm_error_to_io_error)?;
        self.ptr = self.ptr.wrapping_add(len);
        self.len -= len;
        Ok(len)
    }

    /// Flushes the buffer. This is a no-op for `VmBytesMut`.
    fn flush(&mut self) -> ax_io::Result {
        Ok(())
    }
}

impl IoBufMut for VmBytesMut<'_> {
    fn remaining_mut(&self) -> usize {
        self.len
    }
}

/// Patches kernel text, ensuring page permissions and instruction-cache
/// synchronization are handled consistently.
pub fn patch_kernel_text<F>(addr: VirtAddr, len: usize, action: F) -> StarryResult<()>
where
    F: FnOnce(*mut u8),
{
    if len == 0 {
        return Ok(());
    }

    let aligned_addr = addr.align_down_4k();
    let aligned_length = (addr + len).align_up_4k() - aligned_addr;

    // Acquire the kernel address-space IRQ-safe lock only after the task-context
    // stopper coordinator has parked every remote CPU. The coordinator lock is
    // sleepable and is acquired before IRQs are disabled; the page-table guard
    // is therefore the only IRQ-saving lock nested inside the stopped region.
    // Keeping that order avoids restoring saved IRQ state out of order.
    crate::stop_machine::stop_machine(
        move || -> StarryResult<()> {
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
                Err(StarryError::BadAddress)
            }
        },
        move || sync_modified_kernel_text(aligned_addr, aligned_length),
    )
}

/// Writes data to kernel text, ensuring the page permissions are properly handled.
pub fn write_kernel_text(addr: VirtAddr, data: &[u8]) -> StarryResult<()> {
    patch_kernel_text(addr, data.len(), |dst| unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
    })
}

pub fn flush_tlb_range(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::flush_tlb_range(start, size);
}

pub(crate) fn flush_tlb_range_on_cpus_sync(
    cpu_mask: usize,
    start: VirtAddr,
    size: usize,
) -> crate::StarryResult {
    ax_runtime::hal::cache::flush_tlb_range_on_cpus(cpu_mask, start, size).map_err(
        |err| match err {
            ax_runtime::hal::cache::TlbShootdownError::CpuOffline
            | ax_runtime::hal::cache::TlbShootdownError::Unsupported => {
                crate::StarryError::Unsupported
            }
            ax_runtime::hal::cache::TlbShootdownError::Timeout => crate::StarryError::TimedOut,
            ax_runtime::hal::cache::TlbShootdownError::Platform => crate::StarryError::Io,
        },
    )
}

fn sync_modified_kernel_text(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::sync_kernel_text(start, size);
}

#[cfg(axtest)]
pub(crate) fn user_pointer_metadata_rules_hold_for_test() -> bool {
    let user_base = USER_SPACE_BASE;
    let user_end = USER_SPACE_BASE + USER_SPACE_SIZE;
    let ptr = UserPtr::<u32>::from(user_base);
    let const_ptr = UserConstPtr::<u64>::from(user_base + 8);
    let default_ptr = UserPtr::<u8>::default();
    let default_const_ptr = UserConstPtr::<u8>::default();
    let cast_ptr = ptr.cast::<u8>();
    let cast_const_ptr = const_ptr.cast::<u8>();

    default_ptr.is_null()
        && !ptr.is_null()
        && ptr.address().as_usize() == user_base
        && ptr.as_ptr() as usize == user_base
        && cast_ptr.address().as_usize() == user_base
        && const_ptr.address().as_usize() == user_base + 8
        && cast_const_ptr.address().as_usize() == user_base + 8
        // Default const pointer is also null.
        && default_const_ptr.is_null()
        && !const_ptr.is_null()
        // UserPtr/UserConstPtr From<usize> round-trips through address().
        && UserPtr::<u64>::from(user_end - 8).address().as_usize() == user_end - 8
        && UserConstPtr::<u64>::from(user_end - 8).address().as_usize() == user_end - 8
        // check_access accepts zero-length access anywhere in user space,
        // including exactly at USER_SPACE_BASE and one byte before USER_SPACE_END.
        && check_access(user_base, 0).is_ok()
        && check_access(user_end - 1, 0).is_ok()
        && check_access(user_end, 0).is_err()
        // check_access rejects start below USER_SPACE_BASE even for zero length.
        && check_access(user_base - 1, 0).is_err()
        && check_access(0, 0).is_err()
        && check_access(user_base, 4096).is_ok()
        && check_access(user_end - 1, 1).is_ok()
        && check_access(user_base - 1, 1).is_err()
        && check_access(user_end, 0).is_err()
        && check_access(user_end - 1, 2).is_err()
        // Lengths that would wrap the end pointer are rejected.
        && check_access(user_base, USER_SPACE_SIZE).is_ok()
        && check_access(user_base, USER_SPACE_SIZE + 1).is_err()
        && check_access(user_end - 1, usize::MAX).is_err()
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
