use alloc::string::String;
use core::{
    alloc::Layout,
    ffi::c_char,
    hint::{spin_loop, unlikely},
    marker::PhantomData,
    mem::{MaybeUninit, size_of, transmute},
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ax_io::{IoError, prelude::*};
use ax_memory_addr::PAGE_SIZE_4K;
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::{
    cpu::{
        UserAccessError, UserAccessType, UserAtomicError, UserAtomicU32Op,
        asm::user_copy,
        trap::PageFaultFlags,
        user_atomic_u32, user_read_u32,
    },
    paging::MappingFlags,
};
use ax_task::{current, might_sleep};
use extern_trait::extern_trait;
use starry_vm::{VmError, VmIo, VmResult, vm_load_until_nul, vm_read_slice, vm_write_slice};

use crate::{
    StarryError, StarryResult,
    mm::UserVirtualAddressLayout,
    task::{AsThread, Thread},
};

/// Restores the thread's previous user-access state when one copy scope ends.
///
/// The previous value, rather than an unconditional `false`, makes nested
/// helpers safe: an inner copy cannot make the outer faultable region appear
/// inactive to the kernel page-fault handler.
struct UserMemoryAccessScope<'a> {
    thread: &'a Thread,
    previous: bool,
}

impl<'a> UserMemoryAccessScope<'a> {
    fn enter(thread: &'a Thread) -> Self {
        let previous = thread.is_accessing_user_memory();
        thread.set_accessing_user_memory(true);
        Self { thread, previous }
    }
}

impl Drop for UserMemoryAccessScope<'_> {
    fn drop(&mut self) {
        self.thread.set_accessing_user_memory(self.previous);
    }
}

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

    let _scope = UserMemoryAccessScope::enter(thr);
    f()
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
        let page_end = self
            .end
            .as_usize()
            .checked_add(PAGE_SIZE_4K - 1)?
            & !(PAGE_SIZE_4K - 1);
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

    fn prepare(&self, op: &str) -> VmResult {
        if self.range.is_empty() {
            return Ok(());
        }
        ensure_thread_context(op, self.range.start.as_usize(), self.range.len())?;

        let curr = current();
        let thr = curr.try_as_thread().ok_or(VmError::AccessDenied)?;
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
        let mut aspace = aspace_pin.lock();
        if !aspace.can_access_range(
            self.range.start,
            self.range.len(),
            self.intent.mapping_flags(),
        ) {
            return Err(VmError::AccessDenied);
        }
        aspace
            .populate_area(
                VirtAddr::from(span.start),
                span.end - span.start,
                self.intent.mapping_flags(),
            )
            .map_err(|_| VmError::AccessDenied)
    }

    fn copy_from_user(self, dst: &mut [MaybeUninit<u8>]) -> VmResult {
        debug_assert_eq!(self.intent, UserAccessIntent::Read);
        debug_assert_eq!(self.range.len(), dst.len());
        self.prepare("read")?;
        if self.range.is_empty() {
            return Ok(());
        }
        let failed_at = access_user_memory(|| unsafe {
            user_copy(
                dst.as_mut_ptr().cast(),
                self.range.start.as_usize() as *const u8,
                dst.len(),
            )
        });
        if unlikely(failed_at != 0) {
            Err(VmError::AccessDenied)
        } else {
            Ok(())
        }
    }

    fn copy_to_user(self, src: &[u8]) -> VmResult {
        debug_assert_eq!(self.intent, UserAccessIntent::Write);
        debug_assert_eq!(self.range.len(), src.len());
        self.prepare("write")?;
        if self.range.is_empty() {
            return Ok(());
        }
        let failed_at = access_user_memory(|| unsafe {
            user_copy(
                self.range.start.as_usize() as *mut u8,
                src.as_ptr(),
                src.len(),
            )
        });
        if unlikely(failed_at != 0) {
            Err(VmError::AccessDenied)
        } else {
            Ok(())
        }
    }
}

impl UserAccess<NoFault> {
    fn aligned_u32(address: usize, intent: UserAccessIntent) -> Option<Self> {
        if ax_runtime::hal::irq::in_irq_context()
            || !address.is_multiple_of(size_of::<u32>())
        {
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

    fn atomic_u32(
        self,
        operation: UserAtomicU32Op,
        argument: u32,
    ) -> Result<u32, UserAtomicError> {
        debug_assert_eq!(self.intent, UserAccessIntent::ReadWrite);
        // SAFETY: construction checked alignment and the architecture user
        // range. The nofault exception table handles a concurrent unmap.
        unsafe {
            user_atomic_u32(
                self.range.start.as_usize() as *mut u32,
                operation,
                argument,
            )
        }
    }
}

/// Syscall argument records are much smaller than this. Larger transfers use
/// the locked fault-in path, where the address-space lock is amortized over the
/// copy. The capability is always enabled; unsupported architectures return a
/// probe miss and use the same fallback.
const USER_ACCESS_PROBE_MAX_PAGES: usize = 16;

/// Lock-free eligibility probe for a user range: `true` iff every 4 KiB page
/// is currently present and EL0-permitted for the requested access, so this
/// attempt can skip the address-space lock and `populate_area`.
///
/// A write requires the page present *and* EL0-writable, so a copy-on-write page
/// (present read-only) correctly misses and routes to the slow path where the COW
/// copy happens. Any miss / empty / oversized range / address-space overflow
/// returns `false` and the caller takes the unchanged locked slow path.
fn user_range_probe_ready(range: UserAccessRange, intent: UserAccessIntent) -> bool {
    let Some(span) = range.page_span() else {
        return false;
    };
    if span.pages > USER_ACCESS_PROBE_MAX_PAGES {
        return false;
    }
    // A write access requires the page to be present *and* EL0-writable; a
    // copy-on-write page is present-read-only, so a write probe correctly misses
    // and routes to the slow path where `populate_area` performs the COW copy.
    let architecture_access = intent.architecture_access();

    // IRQs off across the whole probe: `PAR_EL1` is a per-CPU scratch register
    // shared with any interrupt handler that also executes an `AT`. Disabling
    // IRQs guarantees no other `AT` runs on this CPU between our `AT` and the
    // `mrs` that reads the result. The range is capped, so the window is a
    // handful of instructions.
    let _guard = crate::sync::PreemptIrqSaveGuard::new();
    let mut page = span.start;
    while page < span.end {
        // SAFETY: IRQs are disabled for the whole loop by the guard above, which
        // is `user_access_ok_page`'s precondition (`PAR_EL1` not clobbered by a
        // concurrent `AT` on this CPU).
        if !unsafe {
            ax_runtime::hal::cpu::asm::user_access_ok_page(page, architecture_access)
        } {
            return false;
        }
        page += PAGE_SIZE_4K;
    }
    true
}

fn check_region(start: VirtAddr, layout: Layout, access_flags: MappingFlags) -> StarryResult<()> {
    let align = layout.align();
    if start.as_usize() & (align - 1) != 0 {
        return Err(StarryError::BadAddress);
    }

    let intent = if access_flags.contains(MappingFlags::WRITE) {
        UserAccessIntent::ReadWrite
    } else {
        UserAccessIntent::Read
    };
    UserAccess::<Faultable>::new(start.as_usize(), layout.size(), intent)?
        .prepare("atomic update")?;
    Ok(())
}

pub fn atomic_update_user_u32(
    ptr: *mut u32,
    mut update: impl FnMut(u32) -> StarryResult<u32>,
) -> StarryResult<u32> {
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

/// Atomically updates a futex word without invoking the page-fault handler.
pub fn atomic_update_user_u32_nofault(
    ptr: *mut u32,
    operation: UserAtomicU32Op,
    argument: u32,
) -> Result<u32, UserAtomicError> {
    UserAccess::<NoFault>::aligned_u32(ptr.addr(), UserAccessIntent::ReadWrite)
        .ok_or(UserAtomicError::Fault)?
        .atomic_u32(operation, argument)
}

/// Reads a futex word without invoking the page-fault handler.
pub fn read_user_u32_nofault(ptr: *const u32) -> Result<u32, UserAccessError> {
    UserAccess::<NoFault>::aligned_u32(ptr.addr(), UserAccessIntent::Read)
        .ok_or(UserAccessError::Fault)?
        .read_u32()
}

/// Resolves and validates a readable futex word outside futex queue locks.
pub fn fault_in_user_u32_read(ptr: *const u32) -> StarryResult<()> {
    fault_in_user_u32(ptr.addr(), UserAccessIntent::Read)
}

/// Resolves and validates a writable futex word outside futex queue locks.
pub fn fault_in_user_u32_write(ptr: *mut u32) -> StarryResult<()> {
    fault_in_user_u32(ptr.addr(), UserAccessIntent::ReadWrite)
}

fn fault_in_user_u32(address: usize, intent: UserAccessIntent) -> StarryResult<()> {
    if !address.is_multiple_of(size_of::<u32>()) {
        return Err(StarryError::InvalidInput);
    }
    UserAccess::<Faultable>::new(address, size_of::<u32>(), intent)?
        .prepare("fault in futex word")
        .map_err(Into::into)
}

/// Cumulative count of user page faults dispatched to the demand-paging handler.
///
/// Every fault that reaches the address-space `handle_page_fault` call is counted, matching the
/// Linux `pgfault` event in mm/vmstat.c (all minor + major faults, regardless of resolution).
/// Exposed through `/proc/vmstat` so node_exporter's vmstat collector can surface
/// `node_vmstat_pgfault`.
pub static PAGE_FAULT_COUNT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn handle_page_fault(vaddr: VirtAddr, access_flags: PageFaultFlags) -> bool {
    debug!("Page fault at {vaddr:#x}, access_flags: {access_flags:#x?}");

    #[cfg(feature = "stack-guard-page")]
    if ax_task::diagnose_current_stack_guard_page_fault(vaddr) {
        return false;
    }

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        return false;
    };
    let Ok(aspace_arc) = thr.proc_data.pin_aspace() else {
        return false;
    };

    if unlikely(!thr.is_accessing_user_memory()) {
        // User-mode faults are dispatched by task::user. This hook is only
        // for kernel-mode faults raised inside a typed faultable user-copy
        // scope. Treating every kernel dereference in the user range as a
        // valid copy would hide accidental raw user-pointer accesses.
        return false;
    }

    might_sleep();
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        warn!(
            "user page fault while current thread already owns its address-space lock: \
             vaddr={vaddr:#x}, access_flags={access_flags:#x?}"
        );
        return false;
    }
    PAGE_FAULT_COUNT.fetch_add(1, Ordering::Relaxed);
    aspace_arc.handle_page_fault(vaddr, access_flags)
}

pub const PATH_MAX: usize = 4096;

pub fn vm_load_string(ptr: *const c_char) -> StarryResult<String> {
    #[allow(clippy::unnecessary_cast)]
    let bytes = vm_load_until_nul(ptr as *const u8)?;
    String::from_utf8(bytes).map_err(|_| StarryError::IllegalBytes)
}

pub fn vm_load_path_string(ptr: *const c_char) -> StarryResult<String> {
    let path = vm_load_string(ptr)?;
    if path.len() >= PATH_MAX {
        return Err(StarryError::NameTooLong);
    }
    Ok(path)
}

struct Vm;

/// Briefly checks if the given memory region is valid user memory.
pub fn check_access(start: usize, len: usize) -> VmResult {
    let layout = UserVirtualAddressLayout::platform_default()
        .map_err(|_| VmError::AccessDenied)?;
    let user = layout.range();
    let user_base = user.start.as_usize();
    let user_end = user.end.as_usize();
    let ok = (user_base..user_end).contains(&start) && (user_end - start) >= len;
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

/// Faults in and validates a userspace output range without modifying it.
///
/// Transactions use this before their publication point so copyout is the
/// only remaining userspace operation after kernel resources are prepared.
pub(crate) fn prepare_user_write(start: usize, len: usize) -> VmResult {
    // A zero-byte copy has no address capability to validate.  Linux's
    // copy_to_user and iovec paths ignore the pointer in this case, including
    // a null iov_base.
    if len == 0 {
        return Ok(());
    }
    UserAccess::<Faultable>::new(start, len, UserAccessIntent::Write)?.prepare("write")
}

/// Faults in and validates a userspace input range without retaining a user
/// reference.  This is useful when a syscall must validate all iovec segments
/// before it starts an externally visible file operation.
pub(crate) fn prepare_user_read(start: usize, len: usize) -> VmResult {
    // Keep zero-length iovecs out of UserAccessRange construction: their base
    // pointer is semantically unused and may be null.
    if len == 0 {
        return Ok(());
    }
    UserAccess::<Faultable>::new(start, len, UserAccessIntent::Read)?.prepare("read")
}

#[extern_trait]
unsafe impl VmIo for Vm {
    fn new() -> Self {
        Self
    }

    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        // Linux copy_from_user with a zero byte count does not inspect the
        // pointer. Keep this boundary before constructing a checked range so
        // null plus zero remains a successful no-op.
        if buf.is_empty() {
            return Ok(());
        }
        UserAccess::<Faultable>::new(start, buf.len(), UserAccessIntent::Read)?
            .copy_from_user(buf)
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        // Match the zero-length copy_from_user rule above.
        if buf.is_empty() {
            return Ok(());
        }
        UserAccess::<Faultable>::new(start, buf.len(), UserAccessIntent::Write)?
            .copy_to_user(buf)
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
        })
        .map_err(|_| IoError::BadAddress)?;
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
        vm_write_slice(self.ptr, &buf[..len]).map_err(|_| IoError::BadAddress)?;
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
pub fn patch_kernel_text<F>(addr: VirtAddr, len: usize, action: F) -> StarryResult<()>
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
    #[cfg(all(test, not(axtest)))]
    use alloc::vec::Vec;
    #[cfg(all(test, not(axtest)))]
    use core::{mem::MaybeUninit, ptr::NonNull};

    #[cfg(all(test, not(axtest)))]
    use starry_vm::{VmMutPtr, VmPtr, vm_load};

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
        assert_eq!(
            UserAccessIntent::Read.mapping_flags(),
            MappingFlags::READ
        );
        assert_eq!(
            UserAccessIntent::Write.mapping_flags(),
            MappingFlags::WRITE
        );
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
    fn vm_pointer_access_rejects_unmapped_addresses() {
        let null_ptr = core::ptr::null::<u32>();
        assert!(null_ptr.nullable().is_none());

        let dangling = NonNull::<u32>::dangling();
        assert!(dangling.nullable().is_some());
        assert_eq!(dangling.as_ptr().vm_read(), Err(VmError::AccessDenied));
        assert_eq!(dangling.vm_write(42), Err(VmError::AccessDenied));
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn vm_slice_access_rejects_invalid_user_ranges() {
        let mut one_byte = [MaybeUninit::<u8>::uninit()];
        assert_eq!(
            vm_read_slice(core::ptr::null::<u8>(), &mut one_byte),
            Err(VmError::AccessDenied)
        );
        assert_eq!(
            vm_write_slice(core::ptr::null_mut::<u8>(), &[1]),
            Err(VmError::AccessDenied)
        );
        assert_eq!(vm_write_slice(core::ptr::null_mut::<u8>(), &[]), Ok(()));
        assert_eq!(vm_read_slice(core::ptr::null::<u8>(), &mut []), Ok(()));
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn zero_length_iovec_prepare_ignores_its_base_pointer() {
        assert_eq!(prepare_user_read(0, 0), Ok(()));
        assert_eq!(prepare_user_write(usize::MAX, 0), Ok(()));
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn vm_alloc_helpers_validate_inputs_before_copying() {
        let mut unaligned = [0_u16; 2];
        let unaligned_ptr = unaligned
            .as_mut_ptr()
            .cast::<u8>()
            .wrapping_add(1)
            .cast::<u16>();
        assert_eq!(vm_load_until_nul(unaligned_ptr), Err(VmError::BadAddress));
        assert_eq!(
            vm_load(core::ptr::null::<u8>(), 1),
            Err(VmError::AccessDenied)
        );

        let empty: Vec<u8> = vm_load(core::ptr::null::<u8>(), 0).unwrap();
        assert!(empty.is_empty());
    }
}
