use alloc::{string::String, vec::Vec};
use core::{
    ffi::c_char,
    hint::unlikely,
    marker::PhantomData,
    mem::{MaybeUninit, size_of, transmute},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_io::prelude::*;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ax_runtime::hal::{
    cpu::{
        UserAccessError, UserAccessType, UserAtomicError, UserAtomicU32Op, asm::user_copy,
        trap::PageFaultFlags, user_atomic_u32, user_read_u32,
    },
    paging::MappingFlags,
};
use bytemuck::{AnyBitPattern, NoUninit};
use starry_vm::{VmError, VmIo, VmResult};

use super::{FaultResult, io::vm_error_to_io_error};
use crate::{
    StarryError, StarryResult,
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
    let _scope = task.as_thread().enter_user_memory_access();
    Ok(f())
}

/// A faultable access may populate memory and sleep before the copy begins.
struct Faultable;

/// A nofault access is limited to architecture exception-table operations.
struct NoFault;

/// Direction and permission requirements of one user-memory operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserAccessIntent {
    Read,
    Write,
    ReadWrite,
}

impl UserAccessIntent {
    const fn mapping_flags(self) -> MappingFlags {
        match self {
            Self::Read => MappingFlags::READ,
            Self::Write => MappingFlags::WRITE,
            Self::ReadWrite => MappingFlags::READ.union(MappingFlags::WRITE),
        }
    }

    const fn architecture_access(self) -> UserAccessType {
        match self {
            Self::Read => UserAccessType::Read,
            Self::Write | Self::ReadWrite => UserAccessType::Write,
        }
    }
}

/// Checked user range used by both faultable and nofault access modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UserAccessRange {
    start: VirtAddr,
    end: VirtAddr,
}

impl UserAccessRange {
    fn new(start: usize, len: usize) -> VmResult<Self> {
        check_access(start, len)?;
        let end = start.checked_add(len).ok_or(VmError::AccessDenied)?;
        Ok(Self {
            start: VirtAddr::from(start),
            end: VirtAddr::from(end),
        })
    }

    fn len(self) -> usize {
        self.end.as_usize() - self.start.as_usize()
    }

    fn is_empty(self) -> bool {
        self.start.as_usize() == self.end.as_usize()
    }

    fn page_span(self) -> Option<UserPageSpan> {
        if self.is_empty() {
            return None;
        }
        let page_start = self.start.as_usize() & !(PAGE_SIZE_4K - 1);
        let page_end = self.end.as_usize().checked_add(PAGE_SIZE_4K - 1)? & !(PAGE_SIZE_4K - 1);
        let pages = page_end.checked_sub(page_start)? / PAGE_SIZE_4K;
        (pages != 0).then_some(UserPageSpan {
            start: page_start,
            end: page_end,
            pages,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UserPageSpan {
    start: usize,
    end: usize,
    pages: usize,
}

/// A mode-typed, short-lived user-memory operation descriptor.
///
/// This is deliberately not a mapping lease: a concurrent `munmap` may make a
/// successful preparation stale. Faultable copies still run through the
/// architecture exception table, while nofault methods report `Fault`.
struct UserAccess<Mode> {
    range: UserAccessRange,
    intent: UserAccessIntent,
    _mode: PhantomData<Mode>,
}

impl UserAccess<Faultable> {
    fn new(start: usize, len: usize, intent: UserAccessIntent) -> VmResult<Self> {
        Ok(Self {
            range: UserAccessRange::new(start, len)?,
            intent,
            _mode: PhantomData,
        })
    }

    fn prepare(&self, task: &UserTaskRef, _op: &str) -> VmResult {
        if self.range.is_empty() {
            return Ok(());
        }
        if ax_runtime::hal::irq::in_irq_context() {
            return Err(VmError::AccessDenied);
        }
        might_sleep();
        #[cfg(feature = "uaccess-lock-regression")]
        super::record_eager_user_memory_preparation(task);

        let thr = task.as_thread();
        let aspace_pin = thr
            .proc_data
            .pin_aspace()
            .map_err(|_| VmError::AccessDenied)?;
        if unsafe { aspace_pin.raw() }.is_owned_by_current() {
            return Err(VmError::AccessDenied);
        }

        // This is only a present-page optimization decision. It does not pin
        // the mapping; the following copy remains exception-table protected.
        if user_range_probe_ready(self.range, self.intent) {
            return Ok(());
        }

        let span = self.range.page_span().ok_or(VmError::AccessDenied)?;
        if !aspace_pin.lock().can_access_range(
            self.range.start,
            self.range.len(),
            self.intent.mapping_flags(),
        ) {
            return Err(VmError::AccessDenied);
        }
        let access = PageFaultFlags::USER
            | match self.intent {
                UserAccessIntent::Read => PageFaultFlags::READ,
                UserAccessIntent::Write | UserAccessIntent::ReadWrite => PageFaultFlags::WRITE,
            };
        for page in (span.start..span.end).step_by(PAGE_SIZE_4K) {
            // Use the published-MM fault transaction: candidate allocation,
            // file I/O, cancellation and TLB completion all run outside the
            // metadata guard. A competing VMA update is revalidated at apply.
            loop {
                match aspace_pin.handle_page_fault_result(VirtAddr::from(page), access) {
                    FaultResult::Handled => break,
                    // An eviction or shootdown conflict is not EFAULT. Each
                    // retry reacquires the current VMA/PTE snapshot, with no
                    // metadata guard retained while the owner makes progress.
                    FaultResult::Retry => crate::task::yield_now(),
                    _ => return Err(VmError::AccessDenied),
                }
            }
        }
        Ok(())
    }

    fn copy_from_user(self, task: &UserTaskRef, dst: &mut [MaybeUninit<u8>]) -> VmResult {
        debug_assert_eq!(self.intent, UserAccessIntent::Read);
        debug_assert_eq!(self.range.len(), dst.len());
        if self.range.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "uaccess-lock-regression")]
        super::synchronize_user_copy_with_address_space_holder(task);
        // SAFETY: the checked range is user memory, the kernel buffer is valid
        // for its declared length, and the exception table resolves faults.
        let failed_at = access_user_memory(task, || unsafe {
            user_copy(
                dst.as_mut_ptr().cast(),
                self.range.start.as_usize() as *const u8,
                dst.len(),
            )
        })?;
        if unlikely(failed_at != 0) {
            Err(VmError::AccessDenied)
        } else {
            #[cfg(feature = "uaccess-lock-regression")]
            super::record_user_copy_completed(task);
            Ok(())
        }
    }

    fn copy_to_user(self, task: &UserTaskRef, src: &[u8]) -> VmResult {
        debug_assert_eq!(self.intent, UserAccessIntent::Write);
        debug_assert_eq!(self.range.len(), src.len());
        if self.range.is_empty() {
            return Ok(());
        }
        #[cfg(feature = "uaccess-lock-regression")]
        super::synchronize_user_copy_with_address_space_holder(task);
        // SAFETY: the checked range is user memory, the kernel buffer is valid
        // for its declared length, and the exception table resolves faults.
        let failed_at = access_user_memory(task, || unsafe {
            user_copy(
                self.range.start.as_usize() as *mut u8,
                src.as_ptr(),
                src.len(),
            )
        })?;
        if unlikely(failed_at != 0) {
            Err(VmError::AccessDenied)
        } else {
            #[cfg(feature = "uaccess-lock-regression")]
            super::record_user_copy_completed(task);
            Ok(())
        }
    }
}

impl UserAccess<NoFault> {
    fn aligned_u32(address: usize, intent: UserAccessIntent) -> Option<Self> {
        if ax_runtime::hal::irq::in_irq_context() || !address.is_multiple_of(size_of::<u32>()) {
            return None;
        }
        Some(Self {
            range: UserAccessRange::new(address, size_of::<u32>()).ok()?,
            intent,
            _mode: PhantomData,
        })
    }

    fn read_u32(self) -> Result<u32, UserAccessError> {
        debug_assert_eq!(self.intent, UserAccessIntent::Read);
        // SAFETY: construction checked alignment and the architecture user
        // range. The nofault exception table handles a concurrent unmap.
        unsafe { user_read_u32(self.range.start.as_usize() as *const u32) }
    }

    fn atomic_u32(self, operation: UserAtomicU32Op, argument: u32) -> Result<u32, UserAtomicError> {
        debug_assert_eq!(self.intent, UserAccessIntent::ReadWrite);
        // SAFETY: construction checked alignment and the architecture user
        // range. The nofault exception table handles a concurrent unmap.
        unsafe { user_atomic_u32(self.range.start.as_usize() as *mut u32, operation, argument) }
    }
}

/// Syscall argument records are much smaller than this. Larger transfers use
/// the locked fault-in path, where the address-space lock is amortized over the
/// copy. The capability is always enabled; unsupported architectures return a
/// probe miss and use the same fallback.
const USER_ACCESS_PROBE_MAX_PAGES: usize = 16;

/// Lock-free eligibility probe for a user range: `true` iff every 4 KiB page
/// is currently present and EL0-permitted for the requested access, so this
/// attempt can skip the address-space lock and fault transaction.
///
/// A write requires the page present *and* EL0-writable, so a copy-on-write page
/// (present read-only) correctly misses and routes to the slow path where the COW
/// copy happens. Any miss / empty / oversized range / address-space overflow
/// returns `false` and the caller uses the MM's fault transaction.
fn user_range_probe_ready(range: UserAccessRange, intent: UserAccessIntent) -> bool {
    let Some(span) = range.page_span() else {
        return false;
    };
    if span.pages > USER_ACCESS_PROBE_MAX_PAGES {
        return false;
    }
    // A write access requires the page to be present *and* EL0-writable; a
    // copy-on-write page is present-read-only, so a write probe correctly misses
    // and routes to the fault transaction that prepares the COW copy unlocked.
    let architecture_access = intent.architecture_access();

    // IRQs off across the whole probe: `PAR_EL1` is a per-CPU scratch register
    // shared with any interrupt handler that also executes an `AT`. Disabling
    // IRQs guarantees no other `AT` runs on this CPU between our `AT` and the
    // `mrs` that reads the result. The range is capped, so the window is a
    // handful of instructions.
    let _guard = crate::sync::NoPreemptIrqSave::new();
    let mut page = span.start;
    while page < span.end {
        // SAFETY: IRQs are disabled for the whole loop by the guard above, which
        // is `user_access_ok_page`'s precondition (`PAR_EL1` not clobbered by a
        // concurrent `AT` on this CPU).
        if !unsafe { ax_runtime::hal::cpu::asm::user_access_ok_page(page, architecture_access) } {
            return false;
        }
        page += PAGE_SIZE_4K;
    }
    true
}

/// User-pointer operations bound to the explicitly selected task.
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

    /// Copies an ABI record through the explicitly selected task.
    ///
    /// # Safety
    /// Every copied user bit pattern must be a valid `Self::Target`.
    unsafe fn vm_read_any(self, task: &UserTaskRef) -> VmResult<Self::Target> {
        let mut vm = UserMemoryProvider::new(task);
        // SAFETY: the caller supplies the target validity contract.
        unsafe { starry_vm::VmPtr::vm_read_any(self, &mut vm) }
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
#[cfg(feature = "jpeg")]
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

/// Encodes initialized fields into a zero-filled userspace ABI object.
pub(crate) struct AbiFieldWriter<'bytes, T> {
    bytes: &'bytes mut [u8],
    _object: PhantomData<fn() -> T>,
}

impl<T> AbiFieldWriter<'_, T> {
    /// Encodes one initialized field without reading the containing object's
    /// padding bytes.
    pub(crate) fn put_field<U: NoUninit>(
        &mut self,
        offset: usize,
        value: &U,
    ) -> crate::StarryResult<()> {
        let value = bytemuck::bytes_of(value);
        let Some(end) = offset.checked_add(value.len()) else {
            return Err(crate::StarryError::BadAddress);
        };
        let Some(field) = self.bytes.get_mut(offset..end) else {
            return Err(crate::StarryError::BadAddress);
        };
        field.copy_from_slice(value);
        Ok(())
    }
}

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

    /// Copies one kernel-owned value to user memory.
    pub fn write(self, task: &UserTaskRef, value: T) -> crate::StarryResult<()>
    where
        T: NoUninit,
    {
        self.0.vm_write(task, value).map_err(Into::into)
    }

    /// Encodes a userspace ABI object and copies it with one faultable memory
    /// transfer. Bytes not covered by a field remain zero, including padding.
    pub(crate) fn write_abi_fields<const N: usize>(
        self,
        task: &UserTaskRef,
        bytes: &mut [u8; N],
        encode: impl FnOnce(&mut AbiFieldWriter<'_, T>) -> crate::StarryResult<()>,
    ) -> crate::StarryResult<()> {
        if N != size_of::<T>() {
            return Err(crate::StarryError::BadAddress);
        }
        bytes.fill(0);
        encode(&mut AbiFieldWriter {
            bytes,
            _object: PhantomData,
        })?;
        UserPtr::<u8>::from(self.0.cast()).write_slice(task, bytes)
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
    UserAccess::<NoFault>::aligned_u32(ptr.addr(), UserAccessIntent::ReadWrite)
        .ok_or(UserAtomicError::Fault)?
        .atomic_u32(operation, argument)
}

pub fn read_user_u32_nofault(ptr: *const u32) -> Result<u32, UserAccessError> {
    UserAccess::<NoFault>::aligned_u32(ptr.addr(), UserAccessIntent::Read)
        .ok_or(UserAccessError::Fault)?
        .read_u32()
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

pub(crate) fn handle_page_fault(vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
    #[cfg(feature = "stack-guard-page")]
    if ax_runtime::diagnostics::diagnose_current_stack_guard_page_fault(vaddr) {
        return false;
    }

    // This callback handles only faults caused by a user mapping or by the
    // kernel explicitly touching one. Reject unrelated kernel addresses before
    // consulting Starry task identity or entering any sleepable MM path.
    let Ok(layout) = super::UserVirtualAddressLayout::platform_default() else {
        return false;
    };
    if !layout.range().contains(vaddr) {
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

    #[cfg(feature = "uaccess-lock-regression")]
    let _ = super::record_faulting_user_copy(&curr);
    might_sleep();
    let Ok(aspace_arc) = thr.proc_data.pin_aspace() else {
        return false;
    };
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        return false;
    }
    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    aspace_arc.handle_page_fault(vaddr, access_flags)
}

fn resolve_page_fault_user_task(
    lookup: Result<Option<UserTaskRef>, ax_std::os::arceos::task::thread::TaskError>,
) -> Result<Option<UserTaskRef>, ax_std::os::arceos::task::thread::TaskError> {
    match lookup {
        Ok(task) => Ok(task),
        Err(
            ax_std::os::arceos::task::thread::TaskError::NotInitialized
            | ax_std::os::arceos::task::thread::TaskError::NoRunnableThread
            | ax_std::os::arceos::task::thread::TaskError::CpuOwnerBorrowed,
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
    let layout =
        super::UserVirtualAddressLayout::platform_default().map_err(|_| VmError::AccessDenied)?;
    let range = layout.range();
    let end = range.end.as_usize();
    let ok = (range.start.as_usize()..end).contains(&start) && (end - start) >= len;
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
    let intent = if access_flags.contains(MappingFlags::WRITE) {
        UserAccessIntent::ReadWrite
    } else {
        UserAccessIntent::Read
    };
    UserAccess::<Faultable>::new(start, len, intent)?.prepare(task, op)
}

/// Faults in and validates a userspace output range without modifying it.
///
/// Transactions use this before their publication point so copyout is the
/// only remaining userspace operation after kernel resources are prepared.
pub(crate) fn prepare_user_write(task: &UserTaskRef, start: usize, len: usize) -> VmResult {
    if len == 0 {
        return Ok(());
    }
    prepare_user_memory(task, "write", start, len, MappingFlags::WRITE)
}

/// Validates a transaction's captured source range before publication.
pub(crate) fn prepare_user_read(task: &UserTaskRef, start: usize, len: usize) -> VmResult {
    if len == 0 {
        return Ok(());
    }
    UserAccess::<Faultable>::new(start, len, UserAccessIntent::Read)?.prepare(task, "read")
}

/// User-memory capability bound to one live Starry task generation.
pub(crate) struct UserMemoryProvider<'task> {
    task: &'task UserTaskRef,
}

impl<'task> UserMemoryProvider<'task> {
    /// Binds user-memory access to a live Starry task reference.
    pub(crate) const fn new(task: &'task UserTaskRef) -> Self {
        Self { task }
    }
}

// SAFETY: the provider is bound to a live task. Copies validate the user range,
// retain a faultable task scope and use architecture exception-table recovery;
// no borrowed user reference escapes the operation.
unsafe impl VmIo for UserMemoryProvider<'_> {
    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        if buf.is_empty() {
            return Ok(());
        }
        UserAccess::<Faultable>::new(start, buf.len(), UserAccessIntent::Read)?
            .copy_from_user(self.task, buf)
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        if buf.is_empty() {
            return Ok(());
        }
        UserAccess::<Faultable>::new(start, buf.len(), UserAccessIntent::Write)?
            .copy_to_user(self.task, buf)
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

    // Take the IRQ-safe kernel address-space lock inside the stopped action.
    // stop_machine first acquires a sleeping serialization mutex and allocates
    // its command state, then pins the coordinator and enters the IRQ-off
    // phase. Holding kernel_aspace across that preparation could sleep in
    // atomic context; nesting it here also preserves LIFO IRQ restoration.
    crate::stop_machine::stop_machine(
        move || -> StarryResult<()> {
            let mut guard = ax_mm::kernel_aspace().lock();
            if guard.contains_range(aligned_addr, aligned_length) {
                let original_flags = guard.mapping_flags(aligned_addr)?;

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
                Ok(())
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

pub fn flush_tlb_range_sync(start: VirtAddr, size: usize) -> StarryResult {
    ax_runtime::hal::cache::flush_tlb_range_all_cpus(start, size).map_err(|err| match err {
        ax_runtime::hal::cache::TlbShootdownError::CpuOffline
        | ax_runtime::hal::cache::TlbShootdownError::Unsupported => StarryError::Unsupported,
        ax_runtime::hal::cache::TlbShootdownError::Timeout => StarryError::TimedOut,
        ax_runtime::hal::cache::TlbShootdownError::GenerationExhausted => {
            StarryError::Errno(syscalls::Errno::EOVERFLOW)
        }
        ax_runtime::hal::cache::TlbShootdownError::Platform => StarryError::Io,
    })
}

fn sync_modified_kernel_text(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::sync_kernel_text(start, size);
}

#[cfg(all(test, not(axtest)))]
fn user_access_range_rules_hold_for_test() -> bool {
    let user_base = crate::config::USER_SPACE_BASE;
    let user_size = crate::config::USER_SPACE_MAX_SIZE;
    let user_end = user_base + user_size;
    // check_access accepts zero-length access anywhere in user space,
    // including exactly at USER_SPACE_BASE and one byte before USER_SPACE_END.
    check_access(user_base, 0).is_ok()
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
        && check_access(user_base, user_size).is_ok()
        && check_access(user_base, user_size + 1).is_err()
        && check_access(user_end - 1, usize::MAX).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn user_access_range_rules_hold() {
        assert!(user_access_range_rules_hold_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn user_access_page_span_is_checked_and_bounded() {
        let page = crate::config::USER_SPACE_BASE.next_multiple_of(PAGE_SIZE_4K);
        let cross_page = UserAccessRange::new(page + PAGE_SIZE_4K - 1, 2).unwrap();
        assert_eq!(
            cross_page.page_span(),
            Some(UserPageSpan {
                start: page,
                end: page + PAGE_SIZE_4K * 2,
                pages: 2,
            })
        );

        let at_budget = UserAccessRange::new(page, PAGE_SIZE_4K * 16).unwrap();
        assert_eq!(at_budget.page_span().unwrap().pages, 16);
        let above_budget = UserAccessRange::new(page, PAGE_SIZE_4K * 17).unwrap();
        assert_eq!(above_budget.page_span().unwrap().pages, 17);
        assert!(above_budget.page_span().unwrap().pages > USER_ACCESS_PROBE_MAX_PAGES);
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn user_access_intent_preserves_faultable_permissions() {
        assert_eq!(UserAccessIntent::Read.mapping_flags(), MappingFlags::READ);
        assert_eq!(UserAccessIntent::Write.mapping_flags(), MappingFlags::WRITE);
        assert_eq!(
            UserAccessIntent::ReadWrite.mapping_flags(),
            MappingFlags::READ | MappingFlags::WRITE
        );
        assert_eq!(
            UserAccessIntent::Read.architecture_access(),
            UserAccessType::Read
        );
        assert_eq!(
            UserAccessIntent::ReadWrite.architecture_access(),
            UserAccessType::Write
        );
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn nofault_user_read_recovers_unmapped_address() {
        // SAFETY: the address is aligned and belongs to the configured user
        // range. It is intentionally unmapped to exercise exception fixup.
        assert!(matches!(
            unsafe { user_read_u32(crate::config::USER_SPACE_BASE as *const u32) },
            Err(UserAccessError::Fault)
        ));
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn user_access_range_rejects_null_overflow_and_kernel_addresses() {
        assert!(UserAccessRange::new(0, 1).is_err());
        assert!(UserAccessRange::new(usize::MAX, 1).is_err());
        let layout = crate::mm::UserVirtualAddressLayout::platform_default().unwrap();
        assert!(UserAccessRange::new(layout.range().end.as_usize(), 1).is_err());
        assert!(UserAccessRange::new(layout.range().start.as_usize(), usize::MAX).is_err());
    }
}
