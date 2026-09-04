use alloc::string::String;
use core::{
    alloc::Layout,
    ffi::c_char,
    hint::{spin_loop, unlikely},
    mem::{MaybeUninit, size_of, transmute},
    ptr, slice,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use ax_io::{IoError, prelude::*};
#[cfg(feature = "user-access-fastpath")]
use ax_memory_addr::PAGE_SIZE_4K;
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::{
    cpu::{
        UserAccessError, UserAtomicError, UserAtomicU32Op,
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

/// syscall-argument structs are far smaller than this; larger transfers take the
/// slow path, where the aspace lock is amortized over a large copy anyway.
#[cfg(feature = "user-access-fastpath")]
const FASTPATH_MAX_PAGES: usize = 16;

/// Lock-free eligibility probe for a user range: `true` iff every 4 KiB page
/// covering `[start, start+len)` is already present and EL0-permitted for the
/// requested access, so the caller can skip the aspace lock and `populate_area`.
///
/// A write requires the page present *and* EL0-writable, so a copy-on-write page
/// (present read-only) correctly misses and routes to the slow path where the COW
/// copy happens. Any miss / empty / oversized range / address-space overflow
/// returns `false` and the caller takes the unchanged locked slow path.
#[cfg(feature = "user-access-fastpath")]
fn user_range_fast_ok(start: VirtAddr, len: usize, access_flags: MappingFlags) -> bool {
    if len == 0 {
        return false;
    }
    // Checked arithmetic, mirroring the slow path's `VirtAddrRange::try_from_start_size`
    // + page rounding: reject to the slow path on any address-space overflow rather
    // than relying on wrap semantics. `check_region` reaches this with a fully
    // caller-controlled `start` and no prior range bound, so a hostile top-of-space
    // pointer must not overflow here (which would panic under an overflow-checks
    // build); it simply falls through to `can_access_range`, which rejects it.
    let start = start.as_usize();
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    let page_start = start & !(PAGE_SIZE_4K - 1);
    let Some(page_end) = end
        .checked_add(PAGE_SIZE_4K - 1)
        .map(|v| v & !(PAGE_SIZE_4K - 1))
    else {
        return false;
    };
    // `end >= start` and both are rounded the same way, so `page_end >= page_start`.
    let pages = (page_end - page_start) / PAGE_SIZE_4K;
    // `pages == 0` is unreachable here: the `len == 0` early return plus
    // `page_end > page_start` guarantee `pages >= 1`. It is kept as a defensive
    // guard so the range cap still holds if either invariant is later removed.
    if pages == 0 || pages > FASTPATH_MAX_PAGES {
        return false;
    }
    // A write access requires the page to be present *and* EL0-writable; a
    // copy-on-write page is present-read-only, so a write probe correctly misses
    // and routes to the slow path where `populate_area` performs the COW copy.
    let write = access_flags.contains(MappingFlags::WRITE);

    // IRQs off across the whole probe: `PAR_EL1` is a per-CPU scratch register
    // shared with any interrupt handler that also executes an `AT`. Disabling
    // IRQs guarantees no other `AT` runs on this CPU between our `AT` and the
    // `mrs` that reads the result. The range is capped, so the window is a
    // handful of instructions.
    let _guard = crate::sync::PreemptIrqSaveGuard::new();
    let mut page = page_start;
    while page < page_end {
        // SAFETY: IRQs are disabled for the whole loop by the guard above, which
        // is `user_access_ok_page`'s precondition (`PAR_EL1` not clobbered by a
        // concurrent `AT` on this CPU).
        if !unsafe { ax_runtime::hal::cpu::asm::user_access_ok_page(page, write) } {
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

    let curr = current();
    let Some(thr) = curr.try_as_thread() else {
        warn!(
            "reject user region check outside thread context: task={}, start={:#x}, len={}",
            curr.id_name(),
            start.as_usize(),
            layout.size()
        );
        return Err(StarryError::BadAddress);
    };
    let aspace_arc = thr.proc_data.aspace();
    if unsafe { aspace_arc.raw() }.is_owned_by_current() {
        return Err(StarryError::BadAddress);
    }

    // Lock-free fast path: if every page is already present with the requested
    // permission, the later dereference will not fault, so skip the aspace lock
    // and `populate_area`. Misses fall through to the locked slow path.
    #[cfg(feature = "user-access-fastpath")]
    if user_range_fast_ok(start, layout.size(), access_flags) {
        return Ok(());
    }

    let mut aspace = aspace_arc.lock();

    if !aspace.can_access_range(start, layout.size(), access_flags) {
        return Err(StarryError::BadAddress);
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

    pub fn get_as_mut(self) -> StarryResult<&'static mut T> {
        check_region(self.address(), Layout::new::<T>(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { &mut *self.0 })
    }

    pub fn get_as_mut_slice(self, len: usize) -> StarryResult<&'static mut [T]> {
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
    if ax_runtime::hal::irq::in_irq_context()
        || !ptr.addr().is_multiple_of(size_of::<u32>())
        || check_access(ptr.addr(), size_of::<u32>()).is_err()
    {
        return Err(UserAtomicError::Fault);
    }

    // SAFETY: the checks above establish alignment and the architecture user
    // range contract. A concurrent mapping change is redirected through the
    // dedicated nofault exception table.
    unsafe { user_atomic_u32(ptr, operation, argument) }
}

/// Reads a futex word without invoking the page-fault handler.
pub fn read_user_u32_nofault(ptr: *const u32) -> Result<u32, UserAccessError> {
    if ax_runtime::hal::irq::in_irq_context()
        || !ptr.addr().is_multiple_of(size_of::<u32>())
        || check_access(ptr.addr(), size_of::<u32>()).is_err()
    {
        return Err(UserAccessError::Fault);
    }

    // SAFETY: the checks above establish alignment and the architecture user
    // range contract. A concurrent mapping change is redirected through the
    // dedicated nofault exception table.
    unsafe { user_read_u32(ptr) }
}

/// Resolves and validates a readable futex word outside futex queue locks.
pub fn fault_in_user_u32_read(ptr: *const u32) -> StarryResult<()> {
    fault_in_user_u32(ptr.addr(), MappingFlags::READ)
}

/// Resolves and validates a writable futex word outside futex queue locks.
pub fn fault_in_user_u32_write(ptr: *mut u32) -> StarryResult<()> {
    fault_in_user_u32(ptr.addr(), MappingFlags::READ.union(MappingFlags::WRITE))
}

fn fault_in_user_u32(address: usize, access_flags: MappingFlags) -> StarryResult<()> {
    if !address.is_multiple_of(size_of::<u32>()) {
        return Err(StarryError::InvalidInput);
    }
    prepare_user_memory(
        "fault in futex word",
        address,
        size_of::<u32>(),
        access_flags,
    )
    .map_err(Into::into)
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

    pub fn get_as_ref(self) -> StarryResult<&'static T> {
        check_region(self.address(), Layout::new::<T>(), Self::ACCESS_FLAGS)?;
        Ok(unsafe { &*self.0 })
    }

    pub fn get_as_slice(self, len: usize) -> StarryResult<&'static [T]> {
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
    aspace_arc.lock().handle_page_fault(vaddr, access_flags)
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

    // Lock-free fast path: if every page is already present with the requested
    // permission, the copy will not fault, so skip the aspace lock and
    // `populate_area`. Misses fall through to the locked slow path.
    #[cfg(feature = "user-access-fastpath")]
    if user_range_fast_ok(start, len, access_flags) {
        return Ok(());
    }

    let mut aspace = aspace_arc.lock();
    if !aspace.can_access_range(start, len, access_flags) {
        return Err(VmError::AccessDenied);
    }

    aspace
        .populate_area(page_start, page_end - page_start, access_flags)
        .map_err(|_| VmError::AccessDenied)
}

/// Faults in and validates a userspace output range without modifying it.
///
/// Transactions use this before their publication point so copyout is the
/// only remaining userspace operation after kernel resources are prepared.
pub(crate) fn prepare_user_write(start: usize, len: usize) -> VmResult {
    prepare_user_memory("write", start, len, MappingFlags::WRITE)
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
        ax_runtime::hal::cache::TlbShootdownError::Platform => StarryError::Io,
    })
}

fn sync_modified_kernel_text(start: VirtAddr, size: usize) {
    ax_runtime::hal::cache::sync_kernel_text(start, size);
}

#[cfg(all(test, not(axtest)))]
fn user_pointer_metadata_rules_hold_for_test() -> bool {
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
    #[cfg(all(test, not(axtest)))]
    use alloc::vec::Vec;
    #[cfg(all(test, not(axtest)))]
    use core::{mem::MaybeUninit, ptr::NonNull};

    #[cfg(all(test, not(axtest)))]
    use starry_vm::{VmMutPtr, VmPtr, vm_load};

    use super::*;

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn user_pointer_metadata_rules_hold() {
        assert!(user_pointer_metadata_rules_hold_for_test());
    }

    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn nofault_user_read_recovers_unmapped_address() {
        // SAFETY: the address is aligned and belongs to the configured user
        // range. It is intentionally unmapped to exercise exception fixup.
        assert!(matches!(
            unsafe { user_read_u32(USER_SPACE_BASE as *const u32) },
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
