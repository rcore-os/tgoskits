//! KCOV (Kernel Code Coverage) support for fuzzing tools like syzkaller.
//!
//! Exposes `/dev/kcov` as a character device. Userspace opens it, initializes
//! a trace buffer via `KCOV_INIT_TRACE`, mmap's the buffer, enables coverage
//! via `KCOV_ENABLE`, runs the workload, and disables via `KCOV_DISABLE`.

use alloc::sync::Arc;
use core::any::Any;

use ax_errno::AxError;
use ax_hal::{mem::phys_to_virt, paging::PageSize};
use ax_memory_addr::PAGE_SIZE_4K;
use ax_sync::Mutex;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use hashbrown::HashMap;
use lazy_static::lazy_static;
use slab::Slab;

use crate::{
    mm::SharedPages,
    pseudofs::{DeviceMmap, DeviceOps},
    task::AsThread,
};

// ---- ioctl command encoding (Linux uapi) ----

const fn _ioc(dir: u32, ty: u8, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((ty as u32) << 8) | nr | ((size as u32) << 16)
}
const fn _io(ty: u8, nr: u32) -> u32 {
    _ioc(0, ty, nr, 0)
}
const fn _ior(ty: u8, nr: u32, size: usize) -> u32 {
    _ioc(2, ty, nr, size)
}

/// Initialize trace collection with the given buffer size (in u64 words).
pub const KCOV_INIT_TRACE: u32 = _ior(b'c', 1, core::mem::size_of::<u64>());
/// Enable coverage collection for the current thread.
pub const KCOV_ENABLE: u32 = _io(b'c', 100);
/// Disable coverage collection for the current thread.
pub const KCOV_DISABLE: u32 = _io(b'c', 101);

/// Coverage is disabled.
pub const KCOV_MODE_DISABLED: u32 = 0;
/// Trace program counters (PCs).
pub const KCOV_TRACE_PC: u32 = 0x100;
/// Trace comparison operations (not yet implemented).
pub const KCOV_TRACE_CMP: u32 = 0x200;

/// Maximum number of coverage entries in the buffer.
pub const KCOV_MAX_ENTRIES: usize = 64 * 1024;

/// Recursion guard: set to 1 while the trace handler runs so that
/// instrumented code inside `kcov_trace_pc_impl` (and its callees)
/// does not re-enter the tracer. Checked / set / cleared in the
/// naked trampoline.
/// This is a workaround as our build system don't allow
/// per-file/crate compiler args and passing __attribute__((no_sanitize("coverage"))) to llvm directly.
#[used]
static mut IN_KCOV_TRACE: u8 = 0;

/// Safety gate: blocks the trace handler until a thread explicitly enables
/// KCOV via the `KCOV_ENABLE` ioctl (which happens from userspace, long
/// after boot).  Without this check, instrumented edges during early boot
/// call `kcov_trace_pc_impl` → `ax_task::current()` before the scheduler
/// has set the per-CPU task pointer (that happens at the end of
/// `primary_init`, well after the first instrumented code runs).
///
/// `ax_task::current()` then panics with "current task is uninitialized".
/// The panic handler tries `ax_println!`, but UART is not ready yet either:
/// the platform init runs `init_trap` before `console::init_early`, so the
/// UART `LazyInit` is still empty.  That is a double-panic, which rustc's
/// abort guard turns into a `ud2` → #UD (unhandled) → #DF (unhandled) →
/// triple fault → CPU reset → Seabios "Booting from ROM…" → boot again →
/// same crash → infinite reset loop.
///
/// Setting this flag to 1 in `KCOV_ENABLE` is safe because by the time
/// userspace can issue ioctls the boot is complete and all affected
/// subsystems are live.
#[used]
static mut KCOV_ANY_ENABLED: u8 = 0;

// ---- Types ----

/// Per-thread KCOV state, stored in both the global map and the `Thread` struct.
#[derive(Clone)]
pub struct KcovThreadState {
    /// Physical pages backing the shared coverage buffer.
    pub buf_pages: Arc<SharedPages>,
    /// Number of `u64` entries (excluding the leading count word).
    pub buf_entries: usize,
    /// Current trace mode (`KCOV_TRACE_PC`, etc.).
    pub mode: u32,
}

/// Global state for `/dev/kcov`, keyed by thread ID.
pub struct KcovDeviceState {
    /// Per-thread KCOV buffers.
    per_thread: Mutex<Slab<KcovThreadState>>,
    /// TID → slab slot index for fast lookup/removal.
    tid_to_slot: Mutex<HashMap<u32, usize>>,
}

impl KcovDeviceState {
    fn new() -> Self {
        Self {
            per_thread: Mutex::new(Slab::new()),
            tid_to_slot: Mutex::new(HashMap::new()),
        }
    }
}

lazy_static! {
    /// Global KCOV state singleton.
    static ref KCOV_STATE: KcovDeviceState = KcovDeviceState::new();
}

/// Remove the KCOV state for a given thread (called on thread exit).
pub fn disable_for_thread(tid: u32) {
    let mut map = KCOV_STATE.per_thread.lock();
    let mut rev = KCOV_STATE.tid_to_slot.lock();
    if let Some(slot) = rev.remove(&tid) {
        map.remove(slot);
    }
}

// ---- /dev/kcov device ----

/// The `/dev/kcov` character device.
pub struct KcovDevice;

impl DeviceOps for KcovDevice {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }
    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            KCOV_INIT_TRACE => {
                let cover_size = arg;
                if cover_size == 0 || cover_size > KCOV_MAX_ENTRIES {
                    return Err(AxError::InvalidInput);
                }

                // Buffer layout: [count: u64 | pc[0]: u64 | ... | pc[N-1]: u64]
                let total_entries = 1 + cover_size;
                let buf_byte_size = total_entries * core::mem::size_of::<u64>();
                // Round up to a multiple of page size (SharedPages requires aligned size)
                let num_pages = buf_byte_size.div_ceil(PAGE_SIZE_4K);
                let buf_byte_size = num_pages * PAGE_SIZE_4K;
                let pages = Arc::new(
                    SharedPages::new(buf_byte_size, PageSize::Size4K)
                        .map_err(|_| VfsError::InvalidInput)?,
                );

                // Zero the count word at the start of the buffer.
                let base_vaddr = phys_to_virt(pages.phys_pages[0]);
                unsafe {
                    core::ptr::write_volatile(base_vaddr.as_mut_ptr_of::<u64>(), 0u64);
                }

                let state = KcovThreadState {
                    buf_pages: pages,
                    buf_entries: cover_size,
                    mode: KCOV_MODE_DISABLED,
                };

                let task = ax_task::current();
                let tid = task.id().as_u64() as u32;

                let mut map = KCOV_STATE.per_thread.lock();
                let mut rev = KCOV_STATE.tid_to_slot.lock();
                if let Some(&slot) = rev.get(&tid) {
                    map[slot] = state;
                } else {
                    let slot = map.insert(state);
                    rev.insert(tid, slot);
                }
                Ok(0)
            }

            KCOV_ENABLE => {
                let mode = arg as u32;
                if mode != KCOV_TRACE_PC && mode != KCOV_TRACE_CMP {
                    return Err(AxError::InvalidInput);
                }

                // Let the hot path know at least one thread is tracing.
                unsafe {
                    KCOV_ANY_ENABLED = 1;
                }

                let task = ax_task::current();
                let tid = task.id().as_u64() as u32;

                let mut map = KCOV_STATE.per_thread.lock();
                let rev = KCOV_STATE.tid_to_slot.lock();
                let slot = rev.get(&tid).ok_or(VfsError::InvalidInput)?;
                let kcov_state = &mut map[*slot];
                kcov_state.mode = mode;

                // Store in Thread for lock-free access from coverage hook
                if let Some(thr) = task.try_as_thread() {
                    thr.set_kcov(Some(kcov_state.clone()));
                }

                Ok(0)
            }

            KCOV_DISABLE => {
                // Stop recording but keep the buffer in the global map so
                // mmap still works after disable (matching Linux kcov).
                // Cleanup (disable_for_thread) is called only on thread exit.
                let task = ax_task::current();
                let tid = task.id().as_u64() as u32;

                let mut map = KCOV_STATE.per_thread.lock();
                let rev = KCOV_STATE.tid_to_slot.lock();
                if let Some(&slot) = rev.get(&tid) {
                    map[slot].mode = KCOV_MODE_DISABLED;
                }

                if let Some(thr) = task.try_as_thread() {
                    thr.set_kcov(None);
                }

                Ok(0)
            }

            _ => Err(VfsError::NotATty),
        }
    }

    fn mmap(&self, _offset: u64) -> DeviceMmap {
        let task = ax_task::current();
        let tid = task.id().as_u64() as u32;

        let map = KCOV_STATE.per_thread.lock();
        let rev = KCOV_STATE.tid_to_slot.lock();
        if let Some(&slot) = rev.get(&tid) {
            let state = &map[slot];
            DeviceMmap::SharedPages(state.buf_pages.clone())
        } else {
            DeviceMmap::NotConfigured
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

// ---- Coverage collection hook ----

// Architecture-specific naked trampolines.
//
// Each defines `__sanitizer_cov_trace_pc`. A `#[naked]` function has no
// prologue, so the return address is still at the ABI-defined entry-point
// location when the asm runs. The trampoline passes it to `kcov_trace_pc_impl`.

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    core::arch::naked_asm!(
        "cmp byte ptr [rip + {guard}], 0",
        "jne 1f",
        "mov byte ptr [rip + {guard}], 1",
        "mov rdi, [rsp]",
        "call {impl}",
        "mov byte ptr [rip + {guard}], 0",
        "1:",
        "ret",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    core::arch::naked_asm!(
        "adrp x16, {guard}",
        "ldrb w17, [x16, #:lo12:{guard}]",
        "cbnz w17, 1f",
        "mov w17, #1",
        "strb w17, [x16, #:lo12:{guard}]",
        "str x30, [sp, #-16]!",
        "mov x0, x30",
        "bl {impl}",
        "ldr x30, [sp], #16",
        "adrp x16, {guard}",
        "strb wzr, [x16, #:lo12:{guard}]",
        "1:",
        "ret",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    core::arch::naked_asm!(
        "la t0, {guard}",
        "lb t1, 0(t0)",
        "bnez t1, 1f",
        "li t1, 1",
        "sb t1, 0(t0)",
        "addi sp, sp, -16",
        "sd ra, 0(sp)",
        "mv a0, ra",
        "call {impl}",
        "ld ra, 0(sp)",
        "addi sp, sp, 16",
        "la t0, {guard}",
        "sb zero, 0(t0)",
        "1:",
        "ret",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

#[cfg(target_arch = "loongarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn __sanitizer_cov_trace_pc() {
    core::arch::naked_asm!(
        "la.local $t0, {guard}",
        "ld.b $t1, $t0, 0",
        "bnez $t1, 1f",
        "ori $t1, $zero, 1",
        "st.b $t1, $t0, 0",
        "addi.d $sp, $sp, -16",
        "st.d $ra, $sp, 0",
        "ori $a0, $ra, 0",
        "bl {impl}",
        "ld.d $ra, $sp, 0",
        "addi.d $sp, $sp, 16",
        "la.local $t0, {guard}",
        "st.b $zero, $t0, 0",
        "1:",
        "jirl $zero, $ra, 0",
        guard = sym IN_KCOV_TRACE,
        impl = sym kcov_trace_pc_impl,
    );
}

/// Records `pc` (the caller's return address) into the current thread's KCOV
/// coverage buffer. Called from the per-arch `__sanitizer_cov_trace_pc`
/// assembly trampoline.
///
/// This runs in the hot path of every instrumented basic block — it must be
/// lock-free and fast.
extern "C" fn kcov_trace_pc_impl(pc: u64) {
    // Guard integrity check: the naked trampoline must have set this.
    if unsafe { IN_KCOV_TRACE } != 1 {
        panic!("IN_KCOV_TRACE not correctly set!");
    }

    // Fast bail-out: skip all task/thread lookups when no thread has
    // enabled kcov (e.g. during boot, before the test starts tracing).
    if unsafe { KCOV_ANY_ENABLED == 0 } {
        return;
    }

    let task = ax_task::current();
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Some(ref kcov) = thr.kcov() else {
        return;
    };
    if kcov.mode != KCOV_TRACE_PC {
        return;
    }

    let pages = &kcov.buf_pages.phys_pages;
    let entries = kcov.buf_entries;

    // Buffer layout: page 0 starts with [count: u64 | pc[0]: u64 | ...]
    let count_vaddr = phys_to_virt(pages[0]);
    let count_ptr = count_vaddr.as_mut_ptr_of::<u64>();

    // Read current count (userspace may be reading concurrently)
    let idx = unsafe { core::ptr::read_volatile(count_ptr) };
    if idx >= entries as u64 {
        return; // buffer full
    }

    // Write PC to buffer at offset (1 + idx)
    let target_byte_offset = (1 + idx as usize) * core::mem::size_of::<u64>();
    let page_idx = target_byte_offset / PAGE_SIZE_4K;
    let page_off = target_byte_offset % PAGE_SIZE_4K;

    if page_idx < pages.len() {
        let entry_vaddr = phys_to_virt(pages[page_idx]);
        unsafe {
            let entry_ptr = entry_vaddr.as_mut_ptr().add(page_off) as *mut u64;
            core::ptr::write_volatile(entry_ptr, pc);
            // Publish: increment count so userspace sees the new entry
            core::ptr::write_volatile(count_ptr, idx + 1);
        }
    }
}
