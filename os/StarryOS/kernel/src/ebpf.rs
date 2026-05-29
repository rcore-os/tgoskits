#![allow(dead_code)]
use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_sync::Mutex;
use kbpf_basic::{
    linux_bpf::bpf_attr,
    map::{
        BpfMapGetNextKeyArg, BpfMapMeta, BpfMapUpdateArg, PerCpuVariants, PerCpuVariantsOps,
        UnifiedMap, bpf_lookup_elem, bpf_map_create, bpf_map_delete_elem, bpf_map_get_next_key,
        bpf_map_update_elem,
    },
    prog::BpfProgMeta,
};

pub mod aux_impl;
pub mod bpf_insn;
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "riscv64",
    target_arch = "aarch64"
))]
pub mod rbpf_jit;

use aux_impl::StarryAuxImpl;

fn map_err(e: LinuxError) -> AxError {
    match e {
        LinuxError::EPERM => AxError::PermissionDenied,
        LinuxError::ENOENT => AxError::NotFound,
        LinuxError::ENOMEM => AxError::NoMemory,
        LinuxError::EINVAL => AxError::InvalidInput,
        LinuxError::ENOSPC => AxError::StorageFull,
        _ => AxError::Io,
    }
}

#[derive(Debug)]
pub struct StarryPerCpuOps;
impl PerCpuVariantsOps for StarryPerCpuOps {
    fn create<T: Clone + Sync + Send + 'static>(_value: T) -> Option<Box<dyn PerCpuVariants<T>>> {
        None
    }
    fn num_cpus() -> u32 {
        ax_config::plat::MAX_CPU_NUM as u32
    }
}

pub struct BpfProg {
    pub prog_type: u32,
    pub insns: Vec<u8>,
    pub meta: BpfProgMeta,
    pub id: u32,
    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64"
    ))]
    pub jitted: Option<Arc<rbpf_jit::RbpfJitBuffer>>,
}

pub struct BpfFdTable {
    pub maps: BTreeMap<u32, UnifiedMap>,
    pub progs: BTreeMap<u32, BpfProg>,
    pub links: BTreeMap<u32, ()>,
    pub raw_tracepoints: BTreeMap<u32, ()>,
    next_fd: u32,
}

impl BpfFdTable {
    pub const fn new() -> Self {
        Self {
            maps: BTreeMap::new(),
            progs: BTreeMap::new(),
            links: BTreeMap::new(),
            raw_tracepoints: BTreeMap::new(),
            next_fd: 1000,
        }
    }

    pub fn alloc_fd(&mut self) -> u32 {
        let fd = self.next_fd;
        self.next_fd += 1;
        fd
    }
}

pub static BPF_GLOBAL: Mutex<BpfFdTable> = Mutex::new(BpfFdTable::new());

fn handle_map_create(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 24 {
        return Err(AxError::InvalidInput);
    }
    let attr = unsafe { &*(uattr as *const bpf_attr) };
    let meta = BpfMapMeta::try_from(attr).map_err(|_| AxError::InvalidInput)?;
    let map = bpf_map_create::<StarryAuxImpl, StarryPerCpuOps>(meta, None).map_err(map_err)?;
    let mut guard = BPF_GLOBAL.lock();
    let fd = guard.alloc_fd();
    guard.maps.insert(fd, map);
    Ok(fd as isize)
}

fn handle_map_lookup_elem(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 20 {
        return Err(AxError::InvalidInput);
    }
    let attr = unsafe { &*(uattr as *const bpf_attr) };
    let arg = BpfMapUpdateArg::from(attr);
    bpf_lookup_elem::<StarryAuxImpl>(arg).map_err(map_err)?;
    Ok(0)
}

fn handle_map_update_elem(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 28 {
        return Err(AxError::InvalidInput);
    }
    let attr = unsafe { &*(uattr as *const bpf_attr) };
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_update_elem::<StarryAuxImpl>(arg).map_err(map_err)?;
    Ok(0)
}

fn handle_map_delete_elem(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 12 {
        return Err(AxError::InvalidInput);
    }
    let attr = unsafe { &*(uattr as *const bpf_attr) };
    let arg = BpfMapUpdateArg::from(attr);
    bpf_map_delete_elem::<StarryAuxImpl>(arg).map_err(map_err)?;
    Ok(0)
}

fn handle_map_get_next_key(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 28 {
        return Err(AxError::InvalidInput);
    }
    let attr = unsafe { &*(uattr as *const bpf_attr) };
    let arg = BpfMapGetNextKeyArg::from(attr);
    bpf_map_get_next_key::<StarryAuxImpl>(arg).map_err(map_err)?;
    Ok(0)
}

fn handle_prog_load(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 48 {
        return Err(AxError::InvalidInput);
    }
    let attr = unsafe { &*(uattr as *const bpf_attr) };
    let mut meta = BpfProgMeta::try_from_bpf_attr::<StarryAuxImpl>(attr).map_err(map_err)?;
    let insns_bytes = meta.take_insns().unwrap_or_default();
    if insns_bytes.is_empty() {
        return Err(AxError::InvalidInput);
    }

    let mut guard = BPF_GLOBAL.lock();
    let id = guard.progs.len() as u32;

    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64"
    ))]
    let jitted = {
        let insns_slice = unsafe {
            core::slice::from_raw_parts(
                insns_bytes.as_ptr() as *const crate::ebpf::bpf_insn::BpfInsn,
                insns_bytes.len() / 8,
            )
        };
        let helpers = kbpf_basic::helper::init_helper_functions::<StarryAuxImpl>();
        let mut rbpf_helpers = alloc::collections::BTreeMap::new();
        for (k, v) in helpers {
            rbpf_helpers.insert(k, v as rbpf::ebpf::Helper);
        }
        crate::ebpf::rbpf_jit::try_jit_compile(insns_slice, &rbpf_helpers).map(Arc::new)
    };

    let prog_type = meta.prog_type as u32;
    let prog = BpfProg {
        prog_type,
        insns: insns_bytes,
        meta,
        id,
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64"
        ))]
        jitted,
    };

    let fd = guard.alloc_fd();
    guard.progs.insert(fd, prog);
    Ok(fd as isize)
}

fn handle_raw_tracepoint_open(_uattr: usize, _size: u32) -> AxResult<isize> {
    Err(AxError::InvalidInput)
}

fn handle_prog_attach(_cmd: u64, _uattr: usize, _size: u32) -> AxResult<isize> {
    Err(AxError::InvalidInput)
}

fn handle_link_create(_uattr: usize, _size: u32) -> AxResult<isize> {
    Err(AxError::InvalidInput)
}

pub fn sys_bpf(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    match cmd {
        0 => handle_map_create(uattr, size),
        1 => handle_map_lookup_elem(uattr, size),
        2 => handle_map_update_elem(uattr, size),
        3 => handle_map_delete_elem(uattr, size),
        4 => handle_map_get_next_key(uattr, size),
        5 => handle_prog_load(uattr, size),
        17 => handle_raw_tracepoint_open(uattr, size),
        28 => handle_prog_attach(cmd, uattr, size),
        29 => handle_link_create(uattr, size),
        _ => Err(AxError::InvalidInput),
    }
}

pub fn sys_perf_event_open(
    _attr_ptr: usize,
    _pid: i32,
    _cpu: i32,
    _group_fd: i32,
    _flags: usize,
) -> AxResult<isize> {
    Err(AxError::InvalidInput)
}

pub fn bpf_close_all_fds() {
    let mut guard = BPF_GLOBAL.lock();
    guard.maps.clear();
    guard.progs.clear();
    guard.links.clear();
    guard.raw_tracepoints.clear();
}

pub fn bpf_close_fd(fd: u32) -> AxResult<()> {
    let mut guard = BPF_GLOBAL.lock();
    if guard.maps.remove(&fd).is_some() {
        return Ok(());
    }
    if guard.progs.remove(&fd).is_some() {
        return Ok(());
    }
    if guard.links.remove(&fd).is_some() {
        return Ok(());
    }
    if guard.raw_tracepoints.remove(&fd).is_some() {
        return Ok(());
    }
    Err(AxError::BadFileDescriptor)
}

pub fn bpf_fd_exists(fd: u32) -> bool {
    let guard = BPF_GLOBAL.lock();
    guard.maps.contains_key(&fd)
        || guard.progs.contains_key(&fd)
        || guard.links.contains_key(&fd)
        || guard.raw_tracepoints.contains_key(&fd)
}

pub fn run_bpf_prog(fd: u32, ctx: u64) -> AxResult<u64> {
    let (insns, jitted) = {
        let guard = BPF_GLOBAL.lock();
        let prog = guard.progs.get(&fd).ok_or(AxError::BadFileDescriptor)?;
        #[cfg(any(
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64"
        ))]
        let jitted = prog.jitted.clone();
        #[cfg(not(any(
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64"
        )))]
        let jitted: Option<()> = None;
        (prog.insns.clone(), jitted)
    };

    #[cfg(any(
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64"
    ))]
    if let Some(jitted) = jitted {
        return Ok(jitted.execute(ctx));
    }

    let mut vm = rbpf::EbpfVmRaw::new(Some(&insns)).map_err(|_| AxError::InvalidInput)?;
    let helpers = kbpf_basic::helper::init_helper_functions::<StarryAuxImpl>();
    for (id, func) in helpers {
        vm.register_helper(id, func as rbpf::ebpf::Helper)
            .map_err(|_| AxError::InvalidInput)?;
    }
    let mem = unsafe { core::slice::from_raw_parts_mut(ctx as *mut u8, 4096) };
    vm.execute_program(mem).map_err(|_| AxError::InvalidInput)
}
