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
/// Reset coverage collection (zero the count word). Used by syzkaller in
/// read-only coverage mode; for writable buffers userspace writes 0 to
/// `buf[0]` directly.
pub const KCOV_RESET_TRACE: u32 = _io(b'c', 104);

// Userspace ABI constants (ioctl arguments — match Linux uapi).
/// Trace program counters (PCs).
pub const KCOV_TRACE_PC: u32 = 0;
/// Trace comparison operations (not yet implemented).
pub const KCOV_TRACE_CMP: u32 = 1;

// Internal mode constants.
/// After open — no buffer allocated (per-fd initial state).
pub const KCOV_MODE_DISABLED: u32 = 0;
/// After INIT_TRACE — buffer allocated, waiting for ENABLE.
pub const KCOV_MODE_INIT: u32 = 1;
/// After ENABLE(KCOV_TRACE_PC) — actively recording PCs.
pub const KCOV_MODE_TRACE_PC: u32 = 2;
/// After ENABLE(KCOV_TRACE_CMP) — reserved, not yet implemented.
pub const KCOV_MODE_TRACE_CMP: u32 = 3;

/// Maximum number of coverage entries in the buffer.
///
/// Must be at least 512K (syzkaller's default `kCoverSize`). The Linux
/// kernel defines `KCOV_MAX_ENTRIES = 1 << 24` on 64-bit; 1M gives us
/// an 8 MB buffer ceiling with headroom for fuzzing workloads.
pub const KCOV_MAX_ENTRIES: usize = 1024 * 1024;

// ---- Types ----

/// Per-thread KCOV state, stored on the `Thread` struct for lock-free access
/// from the hot path (`kcov_trace_pc_impl`).
#[derive(Clone)]
pub struct KcovThreadState {
    /// Physical pages backing the shared coverage buffer.
    pub buf_pages: Arc<SharedPages>,
    /// Maximum count value: when `buf[0]` reaches this, the buffer is full.
    /// Equals `cover_size - 1` (i.e. the number of available PC slots).
    /// Matches Linux `kcov->size` semantics where the count stops at `size - 1`.
    pub buf_entries: usize,
    /// Current trace mode (`KCOV_TRACE_PC`, etc.).
    pub mode: u32,
}

/// Per-fd KCOV state, stored in the kernel `File` struct.
///
/// Each `open("/dev/kcov")` creates its own `KcovFdState`, matching Linux
/// behavior where each file description has an independent kcov instance.
pub struct KcovFdState {
    inner: Mutex<KcovFdInner>,
}

struct KcovFdInner {
    mode: u32,
    buf_pages: Option<Arc<SharedPages>>,
    buf_entries: usize,
    /// TID of the thread that enabled kcov on this fd.
    /// Linux stores `kcov->t = current` and verifies on DISABLE.
    tracer_tid: Option<u64>,
}

impl KcovFdState {
    /// Create a new per-fd kcov instance in DISABLED mode.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(KcovFdInner {
                mode: KCOV_MODE_DISABLED,
                buf_pages: None,
                buf_entries: 0,
                tracer_tid: None,
            }),
        }
    }

    /// Handle ioctl commands for this kcov instance.
    pub fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            KCOV_INIT_TRACE => {
                let cover_size = arg;
                if !(2..=KCOV_MAX_ENTRIES).contains(&cover_size) {
                    return Err(VfsError::InvalidInput);
                }

                let mut inner = self.inner.lock();
                // Only one INIT_TRACE per fd (Linux: EBUSY on second call).
                if inner.mode != KCOV_MODE_DISABLED {
                    return Err(VfsError::ResourceBusy);
                }

                // Buffer layout: [count: u64 | pc[0]: u64 | ... | pc[N-1]: u64]
                let total_entries = cover_size;
                let buf_byte_size = total_entries * core::mem::size_of::<u64>();
                let num_pages = buf_byte_size.div_ceil(PAGE_SIZE_4K);
                let aligned_size = num_pages * PAGE_SIZE_4K;
                let pages = Arc::new(
                    SharedPages::new(aligned_size, PageSize::Size4K)
                        .map_err(|_| VfsError::InvalidInput)?,
                );

                // Zero the count word at the start of the buffer.
                let base_vaddr = phys_to_virt(pages.phys_pages[0]);
                unsafe {
                    core::ptr::write_volatile(base_vaddr.as_mut_ptr_of::<u64>(), 0u64);
                }

                inner.mode = KCOV_MODE_INIT;
                inner.buf_pages = Some(pages);
                inner.buf_entries = total_entries - 1;
                Ok(0)
            }

            KCOV_ENABLE => {
                let mode_arg = arg as u32;
                let internal_mode = match mode_arg {
                    KCOV_TRACE_PC => KCOV_MODE_TRACE_PC,
                    KCOV_TRACE_CMP => return Err(VfsError::InvalidInput),
                    _ => return Err(VfsError::InvalidInput),
                };

                let task = ax_task::current();
                let mut inner = self.inner.lock();
                if inner.mode != KCOV_MODE_INIT {
                    // Linux: ENABLE before INIT_TRACE (or double ENABLE) → EINVAL.
                    return Err(VfsError::InvalidInput);
                }

                // Check thread is not already tracing with another fd instance.
                // Linux: a thread can have at most one kcov instance enabled.
                if let Some(thr) = task.try_as_thread()
                    && thr.with_kcov(|k| k.is_some())
                {
                    return Err(VfsError::ResourceBusy);
                }

                inner.mode = internal_mode;
                inner.tracer_tid = Some(task.id().as_u64());

                // Let the hot path know at least one thread is tracing.
                unsafe {
                    KCOV_GLOBAL_GATE = 1;
                }

                // Store snapshot on Thread for lock-free hot path access.
                if let Some(thr) = task.try_as_thread() {
                    let thread_state = KcovThreadState {
                        buf_pages: inner.buf_pages.clone().unwrap(),
                        buf_entries: inner.buf_entries,
                        mode: internal_mode,
                    };
                    thr.set_kcov(Some(thread_state));
                }

                Ok(0)
            }

            KCOV_DISABLE => {
                // Linux: arg must be 0, else EINVAL.
                if arg != 0 {
                    return Err(VfsError::InvalidInput);
                }

                let task = ax_task::current();
                let mut inner = self.inner.lock();

                // Linux: current->kcov != kcov → EINVAL.
                // Catches DISABLE before ENABLE, DISABLE from wrong thread,
                // and DISABLE after DISABLE (all non-tracing states).
                if inner.tracer_tid != Some(task.id().as_u64()) {
                    return Err(VfsError::InvalidInput);
                }

                inner.mode = KCOV_MODE_INIT;
                inner.tracer_tid = None;

                if let Some(thr) = task.try_as_thread() {
                    thr.set_kcov(None);
                }

                Ok(0)
            }

            KCOV_RESET_TRACE => {
                // Linux: arg must be 0, fd must be active, and caller must be tracer.
                if arg != 0 {
                    return Err(VfsError::InvalidInput);
                }
                let task = ax_task::current();
                let inner = self.inner.lock();
                if inner.mode != KCOV_MODE_TRACE_PC && inner.mode != KCOV_MODE_TRACE_CMP {
                    return Err(VfsError::InvalidInput);
                }
                // Upcoming Linux KCOV_RESET_TRACE patch: only the tracer may reset.
                if inner.tracer_tid != Some(task.id().as_u64()) {
                    return Err(VfsError::InvalidInput);
                }
                if let Some(ref pages) = inner.buf_pages {
                    let count_vaddr = phys_to_virt(pages.phys_pages[0]);
                    unsafe {
                        core::ptr::write_volatile(count_vaddr.as_mut_ptr_of::<u64>(), 0u64);
                    }
                }
                Ok(0)
            }

            _ => Err(VfsError::NotATty),
        }
    }

    /// Handle mmap for this kcov instance.
    pub fn mmap(&self, offset: u64) -> DeviceMmap {
        // Linux kcov requires vm_pgoff == 0; non-zero offset is rejected.
        if offset != 0 {
            return DeviceMmap::NotConfigured;
        }
        let inner = self.inner.lock();
        match inner.buf_pages {
            Some(ref pages) => DeviceMmap::SharedPages(pages.clone()),
            None => DeviceMmap::NotConfigured,
        }
    }

    /// Called when the last `File` reference to this fd is dropped.
    ///
    /// The shared fd state (buf_pages, mode, tracer_tid) is always torn
    /// down — this is the final close.  Per-thread kcov state is cleared
    /// *only* when the calling thread is the tracer, preventing a non-
    /// tracer final close (e.g. a child after fork that outlives the
    /// parent) from writing to an unreachable buffer.  Matches Linux
    /// close semantics where a task's `kcov` reference keeps coverage
    /// alive independently of the fd's lifetime.
    pub fn on_close(&self) {
        let mut inner = self.inner.lock();
        if inner.mode == KCOV_MODE_TRACE_PC || inner.mode == KCOV_MODE_TRACE_CMP {
            // Clear per-thread state only if the closing thread is the tracer.
            // If a non-tracer drops the last File ref (e.g. a forked child
            // outliving its parent), the tracer's thread still holds its own
            // Arc<SharedPages> and will keep writing — but userspace can no
            // longer read the buffer since the fd is gone.
            if inner.tracer_tid == Some(ax_task::current().id().as_u64())
                && let Some(thr) = ax_task::current().try_as_thread()
            {
                thr.set_kcov(None);
            }
            inner.mode = KCOV_MODE_INIT;
            inner.buf_pages = None;
            inner.buf_entries = 0;
            inner.tracer_tid = None;
        } else {
            inner.mode = KCOV_MODE_DISABLED;
            inner.buf_pages = None;
            inner.buf_entries = 0;
            inner.tracer_tid = None;
        }
    }
}

// ---- /dev/kcov device (no-op singleton) ----

/// The `/dev/kcov` character device.
///
/// Per-fd ioctl and mmap are handled by `KcovFdState` in the kernel `File`
/// struct, not by this shared `DeviceOps` singleton. All operations are no-ops
/// — they are intercepted before reaching here.
pub struct KcovDevice;

impl DeviceOps for KcovDevice {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn close(&self, _exclusive: bool) {}

    fn ioctl(&self, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }

    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        DeviceMmap::NotConfigured
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

/// Clean up kcov for the given thread (called on thread exit).
///
/// With per-fd state, the `KcovFdState::on_close` handles cleanup when the
/// `File` is dropped. This is a safety net for thread exit — if any kcov
/// reference remains on the `Thread`, it is cleared here.
pub fn disable_for_thread(_tid: u32) {
    if let Some(thr) = ax_task::current().try_as_thread() {
        thr.set_kcov(None);
    }
}

/// Ensure the child starts with kcov disabled after fork.
///
/// Linux kcov(1): "Coverage collection is disabled in the child after fork()."
/// With per-fd state the child's `Thread` is created fresh with no kcov, so
/// this is a no-op safety net.
pub fn on_fork(_child_tid: u32) {}

// ---- Hot path ----

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

    // Borrow the KCOV state through a closure to avoid cloning
    // Arc<SharedPages> on every traced basic block.
    thr.with_kcov(|kcov| {
        let Some(kcov) = kcov else {
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
                // Ordering: write PC first, then smp_wmb(), then publish the
                // count.  Without the barrier a reader on another core sees
                // the new count but stale PC data on weakly-ordered
                // architectures (aarch64, riscv64, loongarch64).
                core::ptr::write_volatile(entry_ptr, pc);
                core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
                core::ptr::write_volatile(count_ptr, idx + 1);
            }
        }
    });
}
