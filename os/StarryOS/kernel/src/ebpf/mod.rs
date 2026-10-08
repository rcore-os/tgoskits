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
/// Largest read `bpf_probe_read` performs, in bytes.
///
/// Linux lets the verifier bound the size instead. Nothing bounds it here, and
/// the helper runs in whatever context its probe fired from — interrupt
/// context included — so a program that asks for gigabytes would stall or
/// overwrite the kernel before anyone could react. One page covers the
/// context samples and protocol headers the helper is used for.
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
    (-14i64) as u64 // -EFAULT
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
