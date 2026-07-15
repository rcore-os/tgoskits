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

use ax_errno::{AxError, AxResult};
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

use crate::task::try_current_user_irq_view;

pub(crate) mod error;
pub mod map;
pub mod prog;
pub mod transform;

pub use transform::EbpfKernelAuxiliary;

use crate::{
    ebpf::{error::BpfResultExt, map::create_map, prog::load_prog},
    file::add_file_like,
    kprobe::KernelRawMutex,
    mm::VmBytes,
    perf::raw_tracepoint::bpf_raw_tracepoint_open,
};

/// The global BPF helper-function table (id → `RawBPFHelperFn`). Populated by
/// `init_ebpf()` at kernel start so map/prog/jit code can resolve helpers
/// without re-running the kbpf-basic init on every call.
pub static BPF_HELPER_FUN_SET: LazyInit<BTreeMap<u32, RawBPFHelperFn>> = LazyInit::new();

/// BPF helper ID for `bpf_probe_read` (legacy, address-space-agnostic).
const BPF_FUNC_PROBE_READ: u32 = 4;
/// BPF helper ID for `bpf_get_current_pid_tgid`. kbpf-basic does not register
/// this helper, so we implement it directly in StarryOS.
const BPF_FUNC_GET_CURRENT_PID_TGID: u32 = 14;
/// BPF helper ID for `bpf_get_current_comm`. kbpf-basic does not register
/// this helper, so we implement it directly in StarryOS.
const BPF_FUNC_GET_CURRENT_COMM: u32 = 16;
/// BPF helper ID for `bpf_probe_read_kernel`. kbpf-basic only registers
/// `bpf_probe_read` so we alias 113 onto the same raw helper.
const BPF_FUNC_PROBE_READ_KERNEL: u32 = 113;

/// `bpf_get_current_pid_tgid()` — returns `(tgid << 32) | tid` of the
/// currently running task, matching the Linux kernel helper ABI.
fn bpf_get_current_pid_tgid(_a: u64, _b: u64, _c: u64, _d: u64, _e: u64) -> u64 {
    let Some(task) = try_current_user_irq_view() else {
        return 0;
    };
    let tgid = task.tgid() as u64;
    let pid = task.tid() as u64;
    (tgid << 32) | pid
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
    // aya emits `bpf_probe_read_kernel` (helper id 113) for reads of kernel
    // context memory — e.g. `TracePointContext::read_at`, which a cooked
    // tracepoint program uses to pull fields out of its sample buffer.
    // kbpf-basic only registers the legacy `bpf_probe_read` (id 4), whose raw
    // reader is address-space-agnostic in this VM, so alias 113 onto it.
    if let Some(&probe_read) = set.get(&BPF_FUNC_PROBE_READ) {
        set.entry(BPF_FUNC_PROBE_READ_KERNEL).or_insert(probe_read);
    }
    // Register helpers that kbpf-basic does not yet provide (#14, #16).
    set.entry(BPF_FUNC_GET_CURRENT_PID_TGID)
        .or_insert(bpf_get_current_pid_tgid);
    set.entry(BPF_FUNC_GET_CURRENT_COMM)
        .or_insert(bpf_get_current_comm);
    BPF_HELPER_FUN_SET.init_once(set);
}

fn read_bpf_attr(uattr: usize, size: u32) -> AxResult<bpf_attr> {
    // Match Linux's bpf(2) ABI: `vec!` zero-initialises the buffer first,
    // so reading only the first `min(size, sizeof(bpf_attr))` bytes from
    // userland leaves any trailing bytes zero. That covers both directions
    // of the ABI compatibility — short userland buffers (older toolchains)
    // are zero-padded, and oversize buffers have their tail dropped.
    let mut buf = vec![0u8; core::mem::size_of::<bpf_attr>()];
    let copy_len = (size as usize).min(buf.len());
    let mut reader = VmBytes::new(uattr as *mut u8, copy_len);
    reader.read(&mut buf[..copy_len])?;
    // SAFETY: bpf_attr is a transparent C union with all-bytes layout; the
    // user-supplied buffer is bytewise-copied into the slot above, and any
    // unread tail bytes remain zero from the `vec![0u8; ..]` initialization.
    let attr = unsafe { core::ptr::read(buf.as_ptr() as *const bpf_attr) };
    Ok(attr)
}

fn handle_map_create(attr: &bpf_attr) -> AxResult<isize> {
    let meta = BpfMapMeta::try_from(attr).into_ax_result()?;
    let map = create_map(meta).into_ax_result()?;
    // Linux always creates bpf object fds with `O_CLOEXEC`
    // (`anon_inode_getfd(..., O_CLOEXEC)` in `kernel/bpf/syscall.c`).
    let fd = add_file_like(Arc::new(map), true)?;
    Ok(fd as isize)
}

fn handle_prog_load(attr: &bpf_attr) -> AxResult<isize> {
    let mut meta = BpfProgMeta::try_from_bpf_attr::<EbpfKernelAuxiliary>(attr).into_ax_result()?;
    debug!("bpf prog load meta: {meta:#?}");
    let prog = load_prog(&mut meta).into_ax_result()?;
    // bpf prog fds are close-on-exec in Linux as well; see `handle_map_create`.
    let fd = add_file_like(Arc::new(prog), true)?;
    Ok(fd as isize)
}

fn handle_map_update(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_update_elem::<EbpfKernelAuxiliary, KernelRawMutex>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_lookup(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_lookup_elem::<EbpfKernelAuxiliary, KernelRawMutex>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_delete(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_delete_elem::<EbpfKernelAuxiliary, KernelRawMutex>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_get_next_key(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapGetNextKeyArg::from(attr);
    bpf_map_get_next_key::<EbpfKernelAuxiliary, KernelRawMutex>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_freeze(attr: &bpf_attr) -> AxResult<isize> {
    let map_fd = unsafe { attr.__bindgen_anon_2.map_fd };
    bpf_map_freeze::<EbpfKernelAuxiliary, KernelRawMutex>(map_fd).into_ax_result()?;
    Ok(0)
}

fn handle_map_lookup_and_delete(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_lookup_and_delete_elem::<EbpfKernelAuxiliary, KernelRawMutex>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_raw_tracepoint_open(attr: &bpf_attr) -> AxResult<isize> {
    let arg =
        BpfRawTracePointArg::try_from_bpf_attr::<EbpfKernelAuxiliary>(attr).into_ax_result()?;
    bpf_raw_tracepoint_open(arg)
}

/// `bpf(2)` syscall entry-point. The numeric command is decoded into the
/// canonical [`bpf_cmd`] enum from `kbpf-basic` (no locally-redefined
/// command constants).
pub fn sys_bpf(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    // Linux's bpf(2) returns -EINVAL for an unknown/unsupported command, not
    // -ENOSYS; mirror that so user-space feature probing sees the expected
    // errno (`AxError::Unsupported` would map to -ENOSYS).
    let cmd = bpf_cmd::try_from(cmd as u32).map_err(|_| {
        warn!("bpf: unrecognized command {cmd}");
        AxError::InvalidInput
    })?;
    let attr = read_bpf_attr(uattr, size)?;
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
            Err(AxError::InvalidInput)
        }
    }
}
