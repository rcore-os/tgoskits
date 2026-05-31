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
    linux_bpf::{bpf_attr, bpf_cmd},
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

/// `bpf(2)` syscall entry-point. The numeric command is decoded into the
/// canonical [`bpf_cmd`] enum from `kbpf-basic` (no locally-redefined
/// command constants).
pub fn sys_bpf(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    let cmd = bpf_cmd::try_from(cmd as u32).map_err(|_| {
        warn!("bpf: unrecognized command {cmd}");
        AxError::Unsupported
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
            Err(AxError::Unsupported)
        }
    }
}
