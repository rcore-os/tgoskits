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
    BpfError,
    helper::RawBPFHelperFn,
    linux_bpf::bpf_attr,
    map::{
        BpfMapGetNextKeyArg, BpfMapMeta, BpfMapUpdateArg, bpf_lookup_elem, bpf_map_delete_elem,
        bpf_map_freeze, bpf_map_get_next_key, bpf_map_lookup_and_delete_elem, bpf_map_update_elem,
    },
    prog::BpfProgMeta,
    raw_tracepoint::BpfRawTracePointArg,
};

pub mod map;
pub mod prog;
pub mod transform;

pub use transform::EbpfKernelAuxiliary;

use crate::{
    ebpf::{map::create_map, prog::load_prog},
    file::add_file_like,
    mm::VmBytes,
    perf::raw_tracepoint::bpf_raw_tracepoint_open,
};

/// The global BPF helper-function table (id → `RawBPFHelperFn`). Populated by
/// `init_ebpf()` at kernel start so map/prog/jit code can resolve helpers
/// without re-running the kbpf-basic init on every call.
pub static BPF_HELPER_FUN_SET: LazyInit<BTreeMap<u32, RawBPFHelperFn>> = LazyInit::new();

/// Initialize the BPF subsystem: build the helper-function table from
/// `kbpf-basic`. Must be called before `sys_bpf(BPF_PROG_LOAD)` so loaded
/// programs can resolve the helper ids referenced in their instructions.
pub fn init_ebpf() {
    let set = kbpf_basic::helper::init_helper_functions::<EbpfKernelAuxiliary>();
    BPF_HELPER_FUN_SET.init_once(set);
}

#[allow(dead_code)]
mod cmd {
    pub const BPF_MAP_CREATE: u64 = 0;
    pub const BPF_MAP_LOOKUP_ELEM: u64 = 1;
    pub const BPF_MAP_UPDATE_ELEM: u64 = 2;
    pub const BPF_MAP_DELETE_ELEM: u64 = 3;
    pub const BPF_MAP_GET_NEXT_KEY: u64 = 4;
    pub const BPF_PROG_LOAD: u64 = 5;
    pub const BPF_OBJ_PIN: u64 = 6;
    pub const BPF_OBJ_GET: u64 = 7;
    pub const BPF_PROG_ATTACH: u64 = 8;
    pub const BPF_PROG_DETACH: u64 = 9;
    pub const BPF_PROG_TEST_RUN: u64 = 10;
    pub const BPF_PROG_GET_NEXT_ID: u64 = 11;
    pub const BPF_MAP_GET_NEXT_ID: u64 = 12;
    pub const BPF_PROG_GET_FD_BY_ID: u64 = 13;
    pub const BPF_MAP_GET_FD_BY_ID: u64 = 14;
    pub const BPF_OBJ_GET_INFO_BY_FD: u64 = 15;
    pub const BPF_PROG_QUERY: u64 = 16;
    pub const BPF_RAW_TRACEPOINT_OPEN: u64 = 17;
    pub const BPF_BTF_LOAD: u64 = 18;
    pub const BPF_BTF_GET_FD_BY_ID: u64 = 19;
    pub const BPF_TASK_FD_QUERY: u64 = 20;
    pub const BPF_MAP_LOOKUP_AND_DELETE_ELEM: u64 = 21;
    pub const BPF_MAP_FREEZE: u64 = 22;
    pub const BPF_BTF_GET_NEXT_ID: u64 = 23;
    pub const BPF_MAP_LOOKUP_BATCH: u64 = 24;
    pub const BPF_MAP_LOOKUP_AND_DELETE_BATCH: u64 = 25;
    pub const BPF_MAP_UPDATE_BATCH: u64 = 26;
    pub const BPF_MAP_DELETE_BATCH: u64 = 27;
    pub const BPF_LINK_CREATE: u64 = 28;
    pub const BPF_LINK_UPDATE: u64 = 29;
    pub const BPF_LINK_GET_FD_BY_ID: u64 = 30;
    pub const BPF_LINK_GET_NEXT_ID: u64 = 31;
    pub const BPF_ENABLE_STATS: u64 = 32;
    pub const BPF_ITER_CREATE: u64 = 33;
    pub const BPF_LINK_DETACH: u64 = 34;
    pub const BPF_PROG_BIND_MAP: u64 = 35;
}

/// Convert a kbpf-basic [`BpfError`] (which is `axerrno::LinuxError` from
/// the standalone `axerrno` crate published on crates.io) into tgoskits'
/// in-tree [`AxError`]. They are conceptually the same error set, but they
/// are distinct rust types: kbpf-basic links the crates.io `axerrno` while
/// the kernel proper uses the vendored `ax-errno` (renamed package).
fn bpf_to_ax_err(e: BpfError) -> AxError {
    use kbpf_basic::BpfError as B;
    match e {
        B::EPERM => AxError::PermissionDenied,
        B::ENOENT => AxError::NotFound,
        B::EINTR => AxError::Interrupted,
        B::EIO => AxError::Io,
        B::ENOMEM => AxError::NoMemory,
        B::EACCES => AxError::PermissionDenied,
        B::EFAULT => AxError::BadAddress,
        B::EBUSY => AxError::ResourceBusy,
        B::EEXIST => AxError::AlreadyExists,
        B::ENODEV => AxError::NoSuchDevice,
        B::EINVAL => AxError::InvalidInput,
        B::ENOSPC => AxError::StorageFull,
        B::ESPIPE => AxError::InvalidInput,
        B::EPIPE => AxError::BrokenPipe,
        B::ERANGE => AxError::InvalidInput,
        B::ENAMETOOLONG => AxError::InvalidInput,
        B::ENOSYS => AxError::Unsupported,
        B::ENOTDIR => AxError::NotADirectory,
        B::EISDIR => AxError::IsADirectory,
        B::EAGAIN => AxError::WouldBlock,
        B::EBADF => AxError::BadFileDescriptor,
        B::EOPNOTSUPP => AxError::Unsupported,
        B::EADDRINUSE => AxError::AddrInUse,
        // axerrno's AxError has no dedicated `AddrNotAvailable`; fold into
        // InvalidInput so the bpf(2) caller sees -EINVAL rather than a
        // panicky default. EADDRNOTAVAIL paths are rare in the BPF helper
        // surface (mostly socket-level), so this is acceptable until we
        // grow a more specific kernel error type.
        B::EADDRNOTAVAIL => AxError::InvalidInput,
        B::ECONNREFUSED => AxError::ConnectionRefused,
        B::ECONNRESET => AxError::ConnectionReset,
        B::ETIMEDOUT => AxError::TimedOut,
        B::ENOTCONN => AxError::NotConnected,
        _ => AxError::InvalidInput,
    }
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
    let meta = BpfMapMeta::try_from(attr).map_err(bpf_to_ax_err)?;
    let map = create_map(meta).map_err(bpf_to_ax_err)?;
    let fd = add_file_like(Arc::new(map), false)?;
    Ok(fd as isize)
}

fn handle_prog_load(attr: &bpf_attr) -> AxResult<isize> {
    let mut meta =
        BpfProgMeta::try_from_bpf_attr::<EbpfKernelAuxiliary>(attr).map_err(bpf_to_ax_err)?;
    let prog = load_prog(&mut meta).map_err(bpf_to_ax_err)?;
    let fd = add_file_like(Arc::new(prog), false)?;
    Ok(fd as isize)
}

fn handle_map_update(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_update_elem::<EbpfKernelAuxiliary>(arg).map_err(bpf_to_ax_err)?;
    Ok(0)
}

fn handle_map_lookup(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_lookup_elem::<EbpfKernelAuxiliary>(arg).map_err(bpf_to_ax_err)?;
    Ok(0)
}

fn handle_map_delete(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_delete_elem::<EbpfKernelAuxiliary>(arg).map_err(bpf_to_ax_err)?;
    Ok(0)
}

fn handle_map_get_next_key(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapGetNextKeyArg::from(attr);
    bpf_map_get_next_key::<EbpfKernelAuxiliary>(arg).map_err(bpf_to_ax_err)?;
    Ok(0)
}

fn handle_map_freeze(attr: &bpf_attr) -> AxResult<isize> {
    let map_fd = unsafe { attr.__bindgen_anon_2.map_fd };
    bpf_map_freeze::<EbpfKernelAuxiliary>(map_fd).map_err(bpf_to_ax_err)?;
    Ok(0)
}

fn handle_map_lookup_and_delete(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_lookup_and_delete_elem::<EbpfKernelAuxiliary>(arg).map_err(bpf_to_ax_err)?;
    Ok(0)
}

fn handle_raw_tracepoint_open(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfRawTracePointArg::try_from_bpf_attr::<EbpfKernelAuxiliary>(attr)
        .map_err(bpf_to_ax_err)?;
    bpf_raw_tracepoint_open(arg)
}

/// `bpf(2)` syscall entry-point.
pub fn sys_bpf(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    let attr = read_bpf_attr(uattr, size)?;
    match cmd {
        cmd::BPF_MAP_CREATE => handle_map_create(&attr),
        cmd::BPF_PROG_LOAD => handle_prog_load(&attr),
        cmd::BPF_RAW_TRACEPOINT_OPEN => handle_raw_tracepoint_open(&attr),
        cmd::BPF_MAP_UPDATE_ELEM => handle_map_update(&attr),
        cmd::BPF_MAP_LOOKUP_ELEM => handle_map_lookup(&attr),
        cmd::BPF_MAP_DELETE_ELEM => handle_map_delete(&attr),
        cmd::BPF_MAP_GET_NEXT_KEY => handle_map_get_next_key(&attr),
        cmd::BPF_MAP_FREEZE => handle_map_freeze(&attr),
        cmd::BPF_MAP_LOOKUP_AND_DELETE_ELEM => handle_map_lookup_and_delete(&attr),
        _ => {
            warn!("bpf: unsupported command {cmd}");
            Err(AxError::Unsupported)
        }
    }
}

/// `perf_event_open(2)` entry. Trampolines into `crate::perf` which holds
/// the dispatcher across kprobe / tracepoint / software / uprobe types.
pub fn sys_perf_event_open(
    attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> AxResult<isize> {
    let mut buf = vec![0u8; core::mem::size_of::<kbpf_basic::linux_bpf::perf_event_attr>()];
    VmBytes::new(attr_uptr as *mut u8, buf.len()).read(&mut buf)?;
    // SAFETY: perf_event_attr is a `repr(C)` POD; the user buffer is copied
    // bytewise above and we treat the result as the structure.
    let attr = unsafe { &*(buf.as_ptr() as *const kbpf_basic::linux_bpf::perf_event_attr) };
    crate::perf::perf_event_open(attr, pid, cpu, group_fd, flags as u32)
}
