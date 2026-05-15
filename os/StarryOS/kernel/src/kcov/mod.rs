//! KCOV (Kernel Code Coverage) support for fuzzing tools like syzkaller.
//!
//! Exposes `/dev/kcov` as a character device. Userspace opens it, initializes
//! a trace buffer via `KCOV_INIT_TRACE`, mmap's the buffer, enables coverage
//! via `KCOV_ENABLE`, runs the workload, and disables via `KCOV_DISABLE`.
//! Currently only KCOV_TRACE_PC is implemented, KCOV_TRACE_CMP is not yet implemented.

use alloc::sync::Arc;
use core::any::Any;

use ax_errno::AxError;
use ax_hal::{kcov::KCOV_GLOBAL_GATE, mem::phys_to_virt, paging::PageSize};
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

// Userspace ABI constants (ioctl arguments — match Linux uapi).
/// Trace program counters (PCs).
pub const KCOV_TRACE_PC: u32 = 0;
/// Trace comparison operations (not yet implemented).
pub const KCOV_TRACE_CMP: u32 = 1;

// Internal mode constants (stored in KcovThreadState.mode).
/// Initial state — no buffer allocated.
pub const KCOV_MODE_DISABLED: u32 = 0;
/// After INIT_TRACE — buffer allocated, waiting for ENABLE.
pub const KCOV_MODE_INIT: u32 = 1;

/// Maximum number of coverage entries in the buffer.
pub const KCOV_MAX_ENTRIES: usize = 64 * 1024;

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

/// Global state for `/dev/kcov`.
struct KcovDeviceState {
    /// All KCOV instances, each created by a KCOV_INIT_TRACE call.
    instances: Slab<KcovThreadState>,
    /// TID → instance slot index (at most one per TID, matching Linux kcov).
    tid_to_instances: HashMap<u32, usize>,
}

impl KcovDeviceState {
    fn new() -> Self {
        Self {
            instances: Slab::new(),
            tid_to_instances: HashMap::new(),
        }
    }
}

lazy_static! {
    /// Serializes access to all KCOV state.
    static ref KCOV_STATE: Mutex<KcovDeviceState> = Mutex::new(KcovDeviceState::new());
}

/// Remove ALL KCOV instances for the given thread (called on thread exit).
pub fn disable_for_thread(tid: u32) {
    let mut state = KCOV_STATE.lock();
    if let Some(slot) = state.tid_to_instances.remove(&tid) {
        state.instances.remove(slot);
    }
}

/// Ensure the child starts with kcov disabled after fork.
///
/// Linux kcov(1): "Coverage collection is disabled in the child after fork()."
pub fn on_fork(child_tid: u32) {
    disable_for_thread(child_tid);
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

    /// Called when the last file descriptor to `/dev/kcov` is closed.
    ///
    /// Cleans up the kcov instance for the current thread so that a subsequent
    /// open+INIT_TRACE creates a fresh one (matching Linux: one INIT_TRACE per
    /// fd, but a new fd may INIT_TRACE again).  Any outstanding mmap'd buffer
    /// remains valid through the Arc<SharedPages> held by the VMA.
    fn close(&self, _exclusive: bool) {
        let task = ax_task::current();
        let tid = task.id().as_u64() as u32;
        disable_for_thread(tid);
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

                let task = ax_task::current();
                let tid = task.id().as_u64() as u32;

                let mut global = KCOV_STATE.lock();
                // Linux kcov(1): only one INIT_TRACE per TID, second call returns EBUSY.
                if global.tid_to_instances.contains_key(&tid) {
                    return Err(AxError::ResourceBusy);
                }

                let state = KcovThreadState {
                    buf_pages: pages,
                    buf_entries: cover_size,
                    mode: KCOV_MODE_INIT,
                };
                let slot = global.instances.insert(state);
                global.tid_to_instances.insert(tid, slot);
                Ok(0)
            }

            KCOV_ENABLE => {
                let mode_arg = arg as u32;
                // Map userspace ABI constant to internal mode.
                let internal_mode = match mode_arg {
                    KCOV_TRACE_PC => KCOV_MODE_TRACE_PC,
                    KCOV_TRACE_CMP => {
                        // KCOV_TRACE_CMP hooks not yet implemented.
                        return Err(AxError::InvalidInput);
                    }
                    _ => return Err(AxError::InvalidInput),
                };

                let task = ax_task::current();
                let tid = task.id().as_u64() as u32;

                // Let the hot path know at least one thread is tracing.
                unsafe {
                    KCOV_GLOBAL_GATE = 1;
                }

                let mut global = KCOV_STATE.lock();
                let slot = global
                    .tid_to_instances
                    .get(&tid)
                    .copied()
                    .ok_or(VfsError::InvalidInput)?;
                let kcov_state = &mut global.instances[slot];

                // Linux kcov(1): "A thread can have at most one kcov instance
                // enabled at a time."  Must be in INIT mode to enable.
                if kcov_state.mode != KCOV_MODE_INIT {
                    return Err(AxError::ResourceBusy);
                }
                kcov_state.mode = internal_mode;

                // Store in Thread for lock-free access from coverage hook
                if let Some(thr) = task.try_as_thread() {
                    thr.set_kcov(Some(kcov_state.clone()));
                }

                Ok(0)
            }

            KCOV_DISABLE => {
                // Stop recording but keep the buffer in the global map so
                // mmap still works after disable (matching Linux kcov).
                // The mode returns to INIT so the same instance can be
                // re-enabled later.  Cleanup (disable_for_thread) is called
                // only on thread exit.
                let task = ax_task::current();
                let tid = task.id().as_u64() as u32;

                let mut global = KCOV_STATE.lock();
                if let Some(&slot) = global.tid_to_instances.get(&tid) {
                    global.instances[slot].mode = KCOV_MODE_INIT;
                }

                if let Some(thr) = task.try_as_thread() {
                    thr.set_kcov(None);
                }

                Ok(0)
            }

            _ => Err(VfsError::NotATty),
        }
    }

    fn mmap(&self, offset: u64) -> DeviceMmap {
        // Linux kcov requires vm_pgoff == 0; non-zero offset is rejected.
        if offset != 0 {
            return DeviceMmap::NotConfigured;
        }

        let task = ax_task::current();
        let tid = task.id().as_u64() as u32;

        let global = KCOV_STATE.lock();
        match global.tid_to_instances.get(&tid).copied() {
            Some(slot) => DeviceMmap::SharedPages(global.instances[slot].buf_pages.clone()),
            None => DeviceMmap::NotConfigured,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

/// Records `pc` (the caller's return address) into the current thread's KCOV
/// coverage buffer. Called from the per-arch `__sanitizer_cov_trace_pc`
/// assembly trampoline (now in `axhal::kcov`).
///
/// This runs in the hot path of every instrumented basic block — it must be
/// lock-free and fast.
///
/// `no_mangle` + `extern "C"` so that the axhal trampoline can resolve this
/// symbol at link time.
#[unsafe(no_mangle)]
extern "C" fn kcov_trace_pc_impl(pc: u64) {
    // Fast bail-out: skip all task/thread lookups when no thread has
    // enabled kcov (e.g. during boot, before the test starts tracing).
    if unsafe { KCOV_GLOBAL_GATE == 0 } {
        return;
    }

    let task = ax_task::current();
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Some(ref kcov) = thr.kcov() else {
        return;
    };
    if kcov.mode != KCOV_MODE_TRACE_PC {
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
