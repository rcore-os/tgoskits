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

pub(crate) mod error;
pub mod map;
pub mod prog;
pub mod transform;
pub(crate) mod bpf_insn;
pub mod ebpf_jit;

pub(crate) type HelperFn = fn(u64, u64, u64, u64, u64) -> u64;

pub use transform::EbpfKernelAuxiliary;

use crate::{
    ebpf::{error::BpfResultExt, map::create_map, prog::load_prog},
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
    let prog = load_prog(&mut meta).into_ax_result()?;
    // bpf prog fds are close-on-exec in Linux as well; see `handle_map_create`.
    let fd = add_file_like(Arc::new(prog), true)?;
    Ok(fd as isize)
}

fn handle_map_update(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_update_elem::<EbpfKernelAuxiliary>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_lookup(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_lookup_elem::<EbpfKernelAuxiliary>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_delete(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_delete_elem::<EbpfKernelAuxiliary>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_get_next_key(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapGetNextKeyArg::from(attr);
    bpf_map_get_next_key::<EbpfKernelAuxiliary>(arg).into_ax_result()?;
    Ok(0)
}

fn handle_map_freeze(attr: &bpf_attr) -> AxResult<isize> {
    let map_fd = unsafe { attr.__bindgen_anon_2.map_fd };
    bpf_map_freeze::<EbpfKernelAuxiliary>(map_fd).into_ax_result()?;
    Ok(0)
}

fn handle_map_lookup_and_delete(attr: &bpf_attr) -> AxResult<isize> {
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_lookup_and_delete_elem::<EbpfKernelAuxiliary>(arg).into_ax_result()?;
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
