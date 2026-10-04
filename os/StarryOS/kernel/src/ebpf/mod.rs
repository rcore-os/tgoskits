//! eBPF runtime: `bpf(2)` dispatcher, map/prog file-likes, and the
//! kernel-auxiliary glue that lets `kbpf-basic` reach into our address space,
//! per-cpu state, and perf-event output path.
//!
//! Ported from `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/bpf/`). The
//! upstream module was named `bpf` and split into `map.rs`, `prog/mod.rs`,
//! `tansform.rs` (typo). We:
//!
//! * rename the module to `ebpf` to align with PR #805 (which already
//!   introduced `os/StarryOS/kernel/src/ebpf.rs` as a stub);
//! * fix the `tansform` → `transform` typo, see commit message;
//! * adapt all imports from `axhal/axalloc/...` to tgoskits' `ax_hal/
//!   ax_alloc/...` package names (per `crate-fork-audit.md §6`).
//!
//! This module supersedes the single-file stub `sys_bpf` introduced in
//! PR #805 (`feat/ebpf-observability`) — see the PR description.

use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec};
use core::mem::MaybeUninit;

use ax_io::Read;
use ax_lazyinit::LazyInit;
use kbpf_basic::{
    helper::RawBPFHelperFn,
    linux_bpf::{bpf_attr, bpf_cmd},
    map::{
        BpfMapGetNextKeyArg, BpfMapMeta, BpfMapUpdateArg, bpf_lookup_elem, bpf_map_delete_elem,
        bpf_map_freeze, bpf_map_get_next_key, bpf_map_lookup_and_delete_elem, bpf_map_update_elem,
    },
    prog::BpfProgMeta,
    raw_tracepoint::BpfRawTracePointArg,
};

use crate::{
    StarryError, StarryResult,
    task::{current_user_task, try_current_user_irq_view},
};

mod device_controller;
pub(crate) mod error;
pub mod map;
pub mod prog;
pub mod transform;
pub(crate) mod verify;

pub use transform::EbpfKernelAuxiliary;

use crate::{
    ebpf::{error::BpfResultExt, map::create_map, prog::load_prog},
    file::add_file_like,
    mm::VmBytes,
    perf::raw_tracepoint::bpf_raw_tracepoint_open,
    sync::RawSpinLockIrqSaveBackend,
};

/// The global BPF helper-function table (id → `RawBPFHelperFn`). Populated by
/// `init_ebpf()` at kernel start so map/prog/jit code can resolve helpers
/// without re-running the kbpf-basic init on every call.
pub static BPF_HELPER_FUN_SET: LazyInit<BTreeMap<u32, RawBPFHelperFn>> = LazyInit::new();

/// BPF helper ID for `bpf_probe_read` (legacy, address-space-agnostic).
const BPF_FUNC_PROBE_READ: u32 = 4;
/// Largest destination `bpf_probe_read` and `bpf_probe_read_user_str` write,
/// in bytes.
///
/// Linux lets the verifier bound the size instead: it proves that
/// `dst..dst + size` lies inside the program's stack or a map value. Nothing
/// proves that here — helpers are called with bare register values, without
/// the calling VM's stack or map bounds — and the helpers run in whatever
/// context their probe fired from, interrupt context included, so a program
/// that asks for gigabytes would stall or overwrite the kernel before anyone
/// could react. One page covers the context samples, protocol headers and
/// path strings the helpers are used for.
const BPF_PROBE_READ_MAX: u64 = 4096;
/// BPF helper ID for `bpf_get_current_pid_tgid`. kbpf-basic does not register
/// this helper, so we implement it directly in StarryOS.
const BPF_FUNC_GET_CURRENT_PID_TGID: u32 = 14;
/// BPF helper ID for `bpf_get_current_comm`. kbpf-basic does not register
/// this helper, so we implement it directly in StarryOS.
const BPF_FUNC_GET_CURRENT_COMM: u32 = 16;
/// BPF helper ID for `bpf_probe_read_kernel`. Both ids name the same read in
/// this VM, and both are installed from [`bpf_probe_read`].
const BPF_FUNC_PROBE_READ_KERNEL: u32 = 113;
/// BPF helper ID for `bpf_probe_read_user_str`. kbpf-basic resolves this id
/// through `string_from_user_cstr`, whose trait shape has no place for the
/// helper's `size`, so its reader scans for the terminator unbounded and a
/// valid bounded read can fail on the mapping state past the requested range.
/// We install our own helper instead, threading `size` down to the reader.
const BPF_FUNC_PROBE_READ_USER_STR: u32 = 114;
/// Stack scratch, in bytes, for one chunk of `bpf_probe_read_user_str`'s
/// user-to-destination copy.
///
/// The copy must run without heap allocation: a probe may have interrupted an
/// allocator critical section on this CPU, and any `Vec` in the chain would
/// spin on the allocator lock that the interrupted code holds (see issue
/// #2761). Reads advance through this buffer and land in the caller's buffer
/// directly, so the size only trades stack use against copy-call count.
const BPF_PROBE_READ_USER_STR_CHUNK: usize = 256;

/// `bpf_probe_read(void *dst, u32 size, const void *unsafe_ptr)` — copies
/// kernel memory into a BPF-visible buffer without resolving a fault.
///
/// kbpf-basic's helper is a plain `memcpy` that always reports success, so an
/// address outside the mapped ranges takes the kernel down instead of failing
/// the helper. Probe programs run from call sites that include interrupt
/// context, where a page fault is not recoverable, so the read has to report
/// the failure instead.
///
/// Returns 0 on success, or `-EFAULT` with the destination zeroed, matching
/// the Linux helper ABI; `size` beyond [`BPF_PROBE_READ_MAX`] is `-EINVAL`.
///
/// The destination is the address the program named. Programs run with the
/// whole address space registered as allowed memory, so this only keeps an
/// inaccessible range from faulting; it does not establish that the program was
/// entitled to write there.
fn bpf_probe_read(dst: u64, size: u64, unsafe_ptr: u64, _c: u64, _e: u64) -> u64 {
    if size > BPF_PROBE_READ_MAX {
        return (-22i64) as u64; // -EINVAL
    }
    let len = size as usize;
    // SAFETY: both ranges come from the BPF program. An inaccessible range is
    // reported as a fault by the exception table instead of reaching the page
    // fault handler.
    let read = unsafe {
        ax_cpu::kernel_access::copy_from_kernel_nofault(
            dst as *mut u8,
            unsafe_ptr as *const u8,
            len,
        )
    };
    if read.is_ok() {
        return 0;
    }

    // Linux clears the destination on failure so that a program which ignores
    // the return value reads zeros rather than whatever the buffer held. A
    // destination that faults here leaves the remainder untouched.
    bpf_zero_dst(dst, len);
    (-14i64) as u64 // -EFAULT
}

/// Zeroes a BPF-visible destination in bounded chunks, tolerating an
/// inaccessible destination by stopping there.
///
/// Both string-reading helpers clear the destination on failure so that a
/// program which ignores the return value reads zeros rather than whatever
/// the buffer held.
fn bpf_zero_dst(dst: u64, len: usize) {
    let zeros = [0u8; 64];
    let mut written = 0;
    while written < len {
        let chunk = (len - written).min(zeros.len());
        // SAFETY: the destination range is the caller's buffer; the source is
        // this frame's zero array.
        let cleared = unsafe {
            ax_cpu::kernel_access::copy_from_kernel_nofault(
                (dst as *mut u8).add(written),
                zeros.as_ptr(),
                chunk,
            )
        };
        if cleared.is_err() {
            break;
        }
        written += chunk;
    }
}

/// `bpf_probe_read_user_str(void *dst, u32 size, const void *unsafe_ptr)` —
/// copies a NUL-terminated string from user memory into a BPF-visible buffer
/// without servicing page faults, matching the Linux helper ABI.
///
/// Probe programs run with local IRQs disabled, where a user page fault
/// cannot be serviced: a non-resident or invalid page inside the requested
/// range fails the read with `-EFAULT` and a zeroed destination instead of
/// being faulted in. `size` bounds the source read, so bytes past
/// `unsafe_ptr + size` are never touched: a longer source string is truncated
/// to `size - 1` bytes with a forced NUL at the last byte, and the returned
/// length includes the terminator, so an empty source returns 1. `size == 0`
/// returns 0 without touching the destination. `size` beyond
/// [`BPF_PROBE_READ_MAX`] is `-EINVAL` before either range is touched, the
/// same bound as `bpf_probe_read`: without a verifier proving that
/// `dst..dst + size` is a stack or map-value object, the cap is what keeps the
/// copy and the failure-path clearing from writing past the program's buffer
/// by more than one page.
///
/// The copy runs chunk-by-chunk through
/// [`BPF_PROBE_READ_USER_STR_CHUNK`] bytes of stack scratch straight into the
/// destination: probe context may have interrupted an allocator critical
/// section, so the whole call chain stays off the heap.
fn bpf_probe_read_user_str(dst: u64, size: u64, unsafe_ptr: u64, _c: u64, _e: u64) -> u64 {
    if size > BPF_PROBE_READ_MAX {
        return (-22i64) as u64; // -EINVAL
    }
    let len = size as usize;
    if len == 0 {
        return 0;
    }

    copy_str_to_dst(dst, len, |offset, max, scratch| {
        next_source_chunk(unsafe_ptr as usize, offset, max, scratch)
    })
}

/// Copies a NUL-terminated string from `next_chunk` into the BPF destination
/// with `strncpy_from_user_nofault` semantics, for a `len` of at least one.
///
/// `next_chunk(offset, max, scratch)` reads at most `max` source bytes at
/// `offset` into `scratch` and returns them, or `None` when they are not
/// accessible. Returns the length including the terminator; `len` on
/// truncation, with a forced terminator at the last byte; or `-EFAULT` with the
/// whole destination zeroed when the source is not accessible.
fn copy_str_to_dst(
    dst: u64,
    len: usize,
    mut next_chunk: impl FnMut(usize, usize, &mut [MaybeUninit<u8>]) -> Option<&[u8]>,
) -> u64 {
    debug_assert!(len > 0);
    let mut scratch = [const { MaybeUninit::uninit() }; BPF_PROBE_READ_USER_STR_CHUNK];
    let mut copied = 0; // bytes already stored into the destination
    while let Some(chunk) = next_chunk(copied, len - copied, &mut scratch) {
        match chunk.iter().position(|byte| *byte == 0) {
            Some(nul) => {
                // Store the bytes up to and including the terminator.
                if store_to_dst(dst, copied, &chunk[..=nul]).is_err() {
                    return (-14i64) as u64; // -EFAULT
                }
                // `strncpy_from_user_nofault` returns the length including
                // the terminator, so an empty source returns 1.
                return (copied + nul + 1) as u64;
            }
            None => {
                if store_to_dst(dst, copied, chunk).is_err() {
                    return (-14i64) as u64; // -EFAULT
                }
                copied += chunk.len();
                if copied == len {
                    // Truncation: the source filled the whole range without a
                    // terminator, so force one at the last byte and return
                    // `size`, as `strncpy_from_user_nofault` does.
                    let terminator = [0];
                    if store_to_dst(dst, len - 1, &terminator).is_err() {
                        return (-14i64) as u64; // -EFAULT
                    }
                    return len as u64;
                }
            }
        }
    }

    // Linux clears the whole destination on failure so that a program which
    // ignores the return value reads zeros rather than whatever the buffer
    // held. A destination that faults here leaves the remainder untouched.
    bpf_zero_dst(dst, len);
    (-14i64) as u64 // -EFAULT
}

/// Reads the next source chunk of a `bpf_probe_read_user_str` copy.
///
/// Returns `None` when the remaining range is not accessible — it wraps the
/// address space, or a page inside it is non-resident or invalid — so the
/// caller reports `-EFAULT` with a zeroed destination.
fn next_source_chunk(
    base: usize,
    offset: usize,
    max: usize,
    scratch: &mut [MaybeUninit<u8>],
) -> Option<&[u8]> {
    let start = base.checked_add(offset)?;
    crate::mm::vm_read_user_chunk_nofault(start, max, scratch).ok()
}

/// Stores `bytes` at `offset` in the BPF destination `dst`.
///
/// Returns the copy error when the destination range is not accessible, so
/// callers can report `-EFAULT` without touching the buffer further.
fn store_to_dst(dst: u64, offset: usize, bytes: &[u8]) -> Result<(), ()> {
    let Some(target) = (dst as usize).checked_add(offset) else {
        return Err(());
    };
    // SAFETY: `dst` names the program's buffer; an inaccessible range is
    // reported as a fault by the exception table instead of reaching the
    // page-fault handler.
    unsafe {
        ax_cpu::kernel_access::copy_from_kernel_nofault(
            target as *mut u8,
            bytes.as_ptr(),
            bytes.len(),
        )
    }
    .map_err(|_| ())
}

/// `bpf_get_current_pid_tgid()` — returns `(tgid << 32) | tid` of the
/// currently running task, matching the Linux kernel helper ABI.
fn bpf_get_current_pid_tgid(_a: u64, _b: u64, _c: u64, _d: u64, _e: u64) -> u64 {
    let task = current_user_task();
    let thread = task.as_thread();
    let view = crate::task::PidView::new(thread.active_pid_namespace());
    let tgid = view
        .visible_process_number(&thread.proc_data.identity())
        .expect("current process is visible in its active PID namespace")
        .get() as u64;
    let tid = view
        .visible_thread_number(&thread.pid_identity())
        .expect("current thread is visible in its active PID namespace")
        .get() as u64;
    (tgid << 32) | tid
}

/// `bpf_get_current_comm(char *buf, u32 size_of_buf)` — copies the current
/// task's comm (name) into `buf` using `strscpy_pad` semantics: at most
/// `size_of_buf - 1` bytes are copied, a NUL terminator is always written,
/// and remaining bytes are zero-padded. Returns 0 on success or `-EINVAL`
/// when `size_of_buf` is 0, matching the Linux kernel helper ABI.
fn bpf_get_current_comm(buf: u64, size_of_buf: u64, _c: u64, _d: u64, _e: u64) -> u64 {
    let size = size_of_buf as usize;
    if buf == 0 {
        return 0;
    }

    let task = try_current_user_irq_view();
    let mut comm = [0; 16];
    let snapshot_len = match task.as_ref() {
        Some(task) => task.copy_comm(&mut comm),
        None => None,
    };
    drop(task);
    let comm_len = match snapshot_len {
        Some(len) => len,
        None => {
            comm.fill(0);
            comm[..6].copy_from_slice(b"kernel");
            6
        }
    };
    let comm_bytes = &comm[..comm_len];

    if size == 0 {
        return (-22i64) as u64; // -EINVAL
    }

    // Copy at most size-1 bytes to leave room for the NUL terminator.
    let copy_len = comm_bytes.len().min(size.saturating_sub(1));

    // SAFETY: `buf` is a kernel-space pointer validated by the eBPF verifier
    // before the helper is invoked.
    unsafe {
        core::ptr::copy_nonoverlapping(comm_bytes.as_ptr(), buf as *mut u8, copy_len);
        // Always NUL-terminate at `buf[copy_len]`.
        (buf as *mut u8).add(copy_len).write(0);
        // Zero-pad the remainder.
        if copy_len + 1 < size {
            core::ptr::write_bytes((buf as *mut u8).add(copy_len + 1), 0, size - copy_len - 1);
        }
    }
    0
}

/// Initialize the BPF subsystem: build the helper-function table from
/// `kbpf-basic`. Must be called before `sys_bpf(BPF_PROG_LOAD)` so loaded
/// programs can resolve the helper ids referenced in their instructions.
pub fn init_ebpf() {
    let mut set = kbpf_basic::helper::init_helper_functions::<EbpfKernelAuxiliary>();
    // Install the fault-safe reader under both ids. `insert` rather than
    // `entry(..).or_insert(..)`: keeping a plain `memcpy` out of the probe path
    // is the point of the replacement, so it has to win the id whatever
    // kbpf-basic registers there itself.
    set.insert(BPF_FUNC_PROBE_READ, bpf_probe_read);
    // aya emits `bpf_probe_read_kernel` (helper id 113) for reads of kernel
    // context memory — e.g. `TracePointContext::read_at`, which a cooked
    // tracepoint program uses to pull fields out of its sample buffer.
    set.insert(BPF_FUNC_PROBE_READ_KERNEL, bpf_probe_read);
    // The string helper needs the caller's `size` to bound the nofault read;
    // kbpf-basic's own registration drops it (see BPF_FUNC_PROBE_READ_USER_STR).
    set.insert(BPF_FUNC_PROBE_READ_USER_STR, bpf_probe_read_user_str);
    // Register helpers that kbpf-basic does not yet provide (#14, #16).
    set.entry(BPF_FUNC_GET_CURRENT_PID_TGID)
        .or_insert(bpf_get_current_pid_tgid);
    set.entry(BPF_FUNC_GET_CURRENT_COMM)
        .or_insert(bpf_get_current_comm);
    BPF_HELPER_FUN_SET.init_once(set);
}

fn read_bpf_attr(
    current: &crate::task::UserTaskRef,
    uattr: usize,
    size: u32,
) -> crate::StarryResult<bpf_attr> {
    // Match Linux's bpf(2) ABI: `vec!` zero-initialises the buffer first,
    // so reading only the first `min(size, sizeof(bpf_attr))` bytes from
    // userland leaves any trailing bytes zero. That covers both directions
    // of the ABI compatibility — short userland buffers (older toolchains)
    // are zero-padded, and oversize buffers have their tail dropped.
    let mut buf = vec![0u8; core::mem::size_of::<bpf_attr>()];
    let copy_len = (size as usize).min(buf.len());
    let mut reader = VmBytes::new(current, uattr as *mut u8, copy_len);
    reader.read(&mut buf[..copy_len])?;
    // SAFETY: bpf_attr is a transparent C union with all-bytes layout; the
    // user-supplied buffer is bytewise-copied into the slot above, and any
    // unread tail bytes remain zero from the `vec![0u8; ..]` initialization.
    let attr = unsafe { core::ptr::read(buf.as_ptr() as *const bpf_attr) };
    Ok(attr)
}

fn handle_map_create(attr: &bpf_attr) -> StarryResult<isize> {
    let meta = BpfMapMeta::try_from(attr).into_starry_result()?;
    let map = create_map(meta).into_starry_result()?;
    // Linux always creates bpf object fds with `O_CLOEXEC`
    // (`anon_inode_getfd(..., O_CLOEXEC)` in `kernel/bpf/syscall.c`).
    let fd = add_file_like(Arc::new(map), true)?;
    Ok(fd as isize)
}

fn handle_prog_load(attr: &bpf_attr) -> StarryResult<isize> {
    let mut meta =
        BpfProgMeta::try_from_bpf_attr::<EbpfKernelAuxiliary>(attr).into_starry_result()?;
    debug!("bpf prog load meta: {meta:#?}");
    let prog = load_prog(&mut meta).into_starry_result()?;
    // bpf prog fds are close-on-exec in Linux as well; see `handle_map_create`.
    let fd = add_file_like(Arc::new(prog), true)?;
    Ok(fd as isize)
}

fn handle_map_update(attr: &bpf_attr) -> StarryResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_update_elem::<EbpfKernelAuxiliary, RawSpinLockIrqSaveBackend>(arg)
        .into_starry_result()?;
    Ok(0)
}

fn handle_map_lookup(attr: &bpf_attr) -> StarryResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_lookup_elem::<EbpfKernelAuxiliary, RawSpinLockIrqSaveBackend>(arg).into_starry_result()?;
    Ok(0)
}

fn handle_map_delete(attr: &bpf_attr) -> StarryResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_delete_elem::<EbpfKernelAuxiliary, RawSpinLockIrqSaveBackend>(arg)
        .into_starry_result()?;
    Ok(0)
}

fn handle_map_get_next_key(attr: &bpf_attr) -> StarryResult<isize> {
    let arg = BpfMapGetNextKeyArg::from(attr);
    bpf_map_get_next_key::<EbpfKernelAuxiliary, RawSpinLockIrqSaveBackend>(arg)
        .into_starry_result()?;
    Ok(0)
}

fn handle_map_freeze(attr: &bpf_attr) -> StarryResult<isize> {
    let map_fd = unsafe { attr.__bindgen_anon_2.map_fd };
    bpf_map_freeze::<EbpfKernelAuxiliary, RawSpinLockIrqSaveBackend>(map_fd)
        .into_starry_result()?;
    Ok(0)
}

fn handle_map_lookup_and_delete(attr: &bpf_attr) -> StarryResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_lookup_and_delete_elem::<EbpfKernelAuxiliary, RawSpinLockIrqSaveBackend>(arg)
        .into_starry_result()?;
    Ok(0)
}

fn handle_raw_tracepoint_open(attr: &bpf_attr) -> StarryResult<isize> {
    let arg =
        BpfRawTracePointArg::try_from_bpf_attr::<EbpfKernelAuxiliary>(attr).into_starry_result()?;
    bpf_raw_tracepoint_open(arg)
}

/// `bpf(2)` syscall entry-point. The numeric command is decoded into the
/// canonical [`bpf_cmd`] enum from `kbpf-basic` (no locally-redefined
/// command constants).
pub fn sys_bpf(
    current: &crate::task::UserTaskRef,
    cmd: u64,
    uattr: usize,
    size: u32,
) -> crate::StarryResult<isize> {
    // Linux's bpf(2) returns -EINVAL for an unknown/unsupported command, not
    // -ENOSYS; mirror that so user-space feature probing sees the expected
    // errno (`StarryError::Unsupported` would map to -ENOSYS).
    let cmd = bpf_cmd::try_from(cmd as u32).map_err(|_| {
        warn!("bpf: unrecognized command {cmd}");
        StarryError::InvalidInput
    })?;
    let attr = read_bpf_attr(current, uattr, size)?;
    // Non-rootless runc programs the cgroup-v2 device controller
    // unconditionally; intercept those commands before the real handlers.
    if let Some(result) = device_controller::try_handle(cmd, &attr, size) {
        return result;
    }
    match cmd {
        bpf_cmd::BPF_MAP_CREATE => handle_map_create(&attr),
        bpf_cmd::BPF_PROG_LOAD => handle_prog_load(&attr),
        bpf_cmd::BPF_RAW_TRACEPOINT_OPEN => handle_raw_tracepoint_open(&attr),
        bpf_cmd::BPF_MAP_UPDATE_ELEM => handle_map_update(&attr),
        bpf_cmd::BPF_MAP_LOOKUP_ELEM => handle_map_lookup(&attr),
        bpf_cmd::BPF_MAP_DELETE_ELEM => handle_map_delete(&attr),
        bpf_cmd::BPF_MAP_GET_NEXT_KEY => handle_map_get_next_key(&attr),
        bpf_cmd::BPF_MAP_FREEZE => handle_map_freeze(&attr),
        bpf_cmd::BPF_MAP_LOOKUP_AND_DELETE_ELEM => handle_map_lookup_and_delete(&attr),
        other => {
            warn!("bpf: unsupported command {other:?}");
            Err(StarryError::InvalidInput)
        }
    }
}

#[cfg(all(test, not(axtest)))]
pub(crate) fn bpf_unknown_command_is_invalid_for_test() -> bool {
    bpf_cmd::try_from(u32::MAX).is_err()
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::{BPF_PROBE_READ_MAX, bpf_probe_read};

    #[test]
    fn bpf_unknown_command_is_invalid() {
        assert!(super::bpf_unknown_command_is_invalid_for_test());
    }

    #[test]
    fn probe_read_copies_a_size_at_the_cap() {
        // The cap is a limit on what is refused, not on what is served: a read
        // of exactly the cap is still copied whole.
        let source = [7u8; BPF_PROBE_READ_MAX as usize];
        let mut destination = [0u8; BPF_PROBE_READ_MAX as usize];
        let read = bpf_probe_read(
            destination.as_mut_ptr() as u64,
            BPF_PROBE_READ_MAX,
            source.as_ptr() as u64,
            0,
            0,
        );
        assert_eq!(read, 0);
        assert_eq!(destination, source);
    }

    #[test]
    fn probe_read_refuses_a_size_beyond_the_cap() {
        // The size has to be decided before either range is touched: a copy
        // first would follow these null pointers into a fault.
        let read = bpf_probe_read(0, BPF_PROBE_READ_MAX + 1, 0, 0, 0);
        assert_eq!(read, (-22i64) as u64);
    }
}

#[cfg(all(test, axtest))]
mod axtests {
    use ax_alloc::alloc_audit::AllocAuditScope;

    use super::{BPF_PROBE_READ_MAX, bpf_probe_read_user_str, copy_str_to_dst};

    /// Runs the helper's copy on a kernel-resident source string, standing in
    /// for a resident user page that the test kernel does not map.
    fn copy_from(source: &[u8], dst: &mut [u8]) -> u64 {
        copy_str_to_dst(dst.as_mut_ptr() as u64, dst.len(), |offset, max, scratch| {
            let len = max.min(scratch.len()).min(source.len() - offset);
            for (slot, byte) in scratch[..len].iter_mut().zip(&source[offset..]) {
                slot.write(*byte);
            }
            // SAFETY: the loop above initialized the first `len` bytes.
            Some(unsafe { scratch[..len].assume_init_ref() })
        })
    }

    #[axtest::axtest]
    fn probe_read_user_str_counts_the_terminator_of_an_empty_string() {
        // strncpy_from_user_nofault returns the copied length including the
        // NUL, so an empty source returns 1 with the terminator stored.
        let mut out = [0x5au8; 8];
        assert_eq!(copy_from(b"\0", &mut out), 1);
        assert_eq!(out[0], 0);

        let mut out = [0x5au8; 8];
        assert_eq!(copy_from(b"ab\0", &mut out), 3);
        assert_eq!(&out[..3], b"ab\0");
    }

    #[axtest::axtest]
    fn probe_read_user_str_fails_and_zeroes_on_a_non_resident_page() {
        // USER_SPACE_BASE belongs to the user range but is unmapped in the
        // test kernel, so the nofault reader must fail the copy.
        let mut out = [0x5au8; 8];
        let ret = bpf_probe_read_user_str(
            out.as_mut_ptr() as u64,
            out.len() as u64,
            crate::config::USER_SPACE_BASE as u64,
            0,
            0,
        );
        assert_eq!(ret as i64, -14); // -EFAULT
        assert!(out.iter().all(|byte| *byte == 0));
    }

    #[axtest::axtest]
    fn probe_read_user_str_does_not_touch_the_heap_allocator() {
        // A probe can fire while the interrupted code holds the heap
        // allocator's lock on this CPU: the helper's first allocation would
        // spin on that lock forever, so its whole call chain must stay off
        // the heap. The probe scope counts this CPU's allocations with local
        // IRQs disabled, so only code between enter and finish contributes.
        let mut out = [0x5au8; 8];
        let probe = AllocAuditScope::enter();
        let ret = bpf_probe_read_user_str(
            out.as_mut_ptr() as u64,
            out.len() as u64,
            crate::config::USER_SPACE_BASE as u64,
            0,
            0,
        );
        let allocations = probe.finish();
        assert_eq!(ret as i64, -14); // -EFAULT
        assert_eq!(allocations, 0);
        assert!(out.iter().all(|byte| *byte == 0));
    }

    #[axtest::axtest]
    fn probe_read_user_str_bounds_the_destination_by_the_cap() {
        // Nothing proves that the destination holds `size` bytes, so a size
        // above the cap is refused before either range is touched; one at the
        // cap is served, and its failure path zeroes exactly `size` bytes.
        // The buffer is allocated before the probe scope.
        let cap = BPF_PROBE_READ_MAX as usize;
        let mut out = alloc::vec![0x5au8; cap + 904];
        let probe = AllocAuditScope::enter();
        let refused = bpf_probe_read_user_str(
            out.as_mut_ptr() as u64,
            out.len() as u64,
            crate::config::USER_SPACE_BASE as u64,
            0,
            0,
        );
        let refused_untouched = out.iter().all(|byte| *byte == 0x5a);
        let served = bpf_probe_read_user_str(
            out.as_mut_ptr() as u64,
            BPF_PROBE_READ_MAX,
            crate::config::USER_SPACE_BASE as u64,
            0,
            0,
        );
        let allocations = probe.finish();
        assert_eq!(refused as i64, -22); // -EINVAL
        assert!(refused_untouched);
        assert_eq!(served as i64, -14); // -EFAULT
        assert_eq!(allocations, 0);
        assert!(out[..cap].iter().all(|byte| *byte == 0));
        assert!(out[cap..].iter().all(|byte| *byte == 0x5a));
    }

    #[axtest::axtest]
    fn probe_read_user_str_keeps_the_size_edges_of_the_linux_abi() {
        // A zero size returns 0 without touching the destination, matching
        // strncpy_from_user_nofault's `count <= 0` path.
        let mut out = [0x5au8; 4];
        let ret = bpf_probe_read_user_str(out.as_mut_ptr() as u64, 0, 0, 0, 0);
        assert_eq!(ret, 0);
        assert!(out.iter().all(|byte| *byte == 0x5a));

        // A size that does not even fit the helper's `u32` argument is far
        // beyond the cap and is refused before any range is touched.
        let ret = bpf_probe_read_user_str(0, 1 << 33, 0, 0, 0);
        assert_eq!(ret as i64, -22); // -EINVAL
    }
}
