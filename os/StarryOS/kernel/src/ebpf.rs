//! eBPF (Extended Berkeley Packet Filter) subsystem for StarryOS.
//!
//! This module provides a complete in-kernel eBPF implementation including:
//!
//! - **Map management**: Array and Hash maps with fd-based lifecycle
//! - **Program loader**: Parses `bpf_attr` with correct mixed u32/u64 byte-offset layout
//! - **Instruction interpreter**: Supports ALU/JMP/MEM instruction classes with
//!   BPF_EXIT and BPF_CALL handling
//! - **Helper functions**: 11 helpers including map operations, probe_read, ktime,
//!   PID/TGID, UID/GID, and perf_event_output
//! - **fd table**: BpfFdTable with close/remove operations and free-fd reuse
//!
//! # Syscall interface
//!
//! Implements `bpf()` syscall commands: MAP_CREATE, LOOKUP, UPDATE, DELETE,
//! GET_NEXT_KEY, PROG_LOAD, PROG_ATTACH, LINK_CREATE, OBJ_CLOSE.

use alloc::vec::Vec;

use ax_errno::{AxError, AxResult};
use ax_sync::spin::SpinNoIrq;

use crate::task::AsThread;

#[allow(dead_code)]
pub(crate) mod bpf_insn {
    pub const BPF_LD: u8 = 0x00;
    pub const BPF_LDX: u8 = 0x01;
    pub const BPF_ST: u8 = 0x02;
    pub const BPF_STX: u8 = 0x03;
    pub const BPF_ALU: u8 = 0x04;
    pub const BPF_JMP: u8 = 0x05;
    pub const BPF_JMP32: u8 = 0x06;
    pub const BPF_ALU64: u8 = 0x07;

    pub const BPF_W: u8 = 0x00;
    pub const BPF_H: u8 = 0x08;
    pub const BPF_B: u8 = 0x10;
    pub const BPF_DW: u8 = 0x18;

    pub const BPF_IMM: u8 = 0x00;
    pub const BPF_ABS: u8 = 0x20;
    pub const BPF_IND: u8 = 0x40;
    pub const BPF_MEM: u8 = 0x60;
    pub const BPF_LEN: u8 = 0x80;
    pub const BPF_MSH: u8 = 0xa0;

    pub const BPF_ADD: u8 = 0x00;
    pub const BPF_SUB: u8 = 0x10;
    pub const BPF_MUL: u8 = 0x20;
    pub const BPF_DIV: u8 = 0x30;
    pub const BPF_OR: u8 = 0x40;
    pub const BPF_AND: u8 = 0x50;
    pub const BPF_LSH: u8 = 0x60;
    pub const BPF_RSH: u8 = 0x70;
    pub const BPF_NEG: u8 = 0x80;
    pub const BPF_MOD: u8 = 0x90;
    pub const BPF_XOR: u8 = 0xa0;
    pub const BPF_MOV: u8 = 0xb0;
    pub const BPF_ARSH: u8 = 0xc0;
    pub const BPF_END: u8 = 0xd0;

    pub const BPF_JA: u8 = 0x00;
    pub const BPF_EXIT: u8 = 0x90;
    pub const BPF_JEQ: u8 = 0x10;
    pub const BPF_JGT: u8 = 0x20;
    pub const BPF_JGE: u8 = 0x30;
    pub const BPF_JSET: u8 = 0x40;
    pub const BPF_JNE: u8 = 0x50;
    pub const BPF_JSGT: u8 = 0x60;
    pub const BPF_JSGE: u8 = 0x70;
    pub const BPF_JLT: u8 = 0xa0;
    pub const BPF_JLE: u8 = 0xb0;
    pub const BPF_JSLT: u8 = 0xc0;
    pub const BPF_JSLE: u8 = 0xd0;

    pub const BPF_K: u8 = 0x00;
    pub const BPF_X: u8 = 0x08;

    pub const BPF_CALL_OP: u8 = 0x80;

    pub const BPF_PSEUDO_MAP_FD: u8 = 1;
    pub const BPF_PSEUDO_MAP_VALUE: u8 = 2;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct BpfInsn {
        pub code: u8,
        pub dst_src_reg: u8,
        pub off: i16,
        pub imm: i32,
    }

    impl BpfInsn {
        pub const fn new(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> Self {
            Self {
                code,
                dst_src_reg: (dst & 0xf) | ((src & 0xf) << 4),
                off,
                imm,
            }
        }

        pub fn dst_reg(&self) -> u8 {
            self.dst_src_reg & 0xf
        }

        pub fn src_reg(&self) -> u8 {
            (self.dst_src_reg >> 4) & 0xf
        }

        pub fn class(&self) -> u8 {
            self.code & 0x07
        }

        pub fn size(&self) -> u8 {
            self.code & 0x18
        }

        pub fn mode(&self) -> u8 {
            self.code & 0xe0
        }

        pub fn alu_op(&self) -> u8 {
            self.code & 0xf0
        }

        pub fn is_ld_dw_imm(&self) -> bool {
            self.code == (BPF_LD | BPF_IMM | BPF_DW)
        }

        pub fn to_bytes(self) -> [u8; 8] {
            let mut buf = [0u8; 8];
            buf[0] = self.code;
            buf[1] = self.dst_src_reg;
            buf[2..4].copy_from_slice(&self.off.to_le_bytes());
            buf[4..8].copy_from_slice(&self.imm.to_le_bytes());
            buf
        }

        pub fn from_bytes(bytes: &[u8; 8]) -> Self {
            Self {
                code: bytes[0],
                dst_src_reg: bytes[1],
                off: i16::from_le_bytes([bytes[2], bytes[3]]),
                imm: i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            }
        }
    }
}

#[allow(dead_code)]
mod map_type {
    pub const UNSPEC: u32 = 0;
    pub const HASH: u32 = 1;
    pub const ARRAY: u32 = 2;
    pub const PROG_ARRAY: u32 = 3;
    pub const PERF_EVENT_ARRAY: u32 = 4;
    pub const PERCPU_HASH: u32 = 5;
    pub const PERCPU_ARRAY: u32 = 6;
    pub const STACK_TRACE: u32 = 7;
    pub const LRU_HASH: u32 = 9;
    pub const LRU_PERCPU_HASH: u32 = 10;
    pub const LPM_TRIE: u32 = 11;
    pub const QUEUE: u32 = 22;
    pub const STACK: u32 = 23;
    pub const RINGBUF: u32 = 27;
}

#[allow(dead_code)]
mod prog_type {
    pub const UNSPEC: u32 = 0;
    pub const SOCKET_FILTER: u32 = 1;
    pub const KPROBE: u32 = 2;
    pub const SCHED_CLS: u32 = 3;
    pub const TRACEPOINT: u32 = 5;
    pub const XDP: u32 = 6;
    pub const PERF_EVENT: u32 = 7;
    pub const CGROUP_SKB: u32 = 8;
    pub const RAW_TRACEPOINT: u32 = 17;
    pub const LSM: u32 = 29;
    pub const SYSCALL: u32 = 31;
}

#[allow(dead_code)]
mod cmd {
    pub const MAP_CREATE: u64 = 0;
    pub const MAP_LOOKUP_ELEM: u64 = 1;
    pub const MAP_UPDATE_ELEM: u64 = 2;
    pub const MAP_DELETE_ELEM: u64 = 3;
    pub const MAP_GET_NEXT_KEY: u64 = 4;
    pub const PROG_LOAD: u64 = 5;
    pub const OBJ_PIN: u64 = 6;
    pub const OBJ_GET: u64 = 7;
    pub const PROG_ATTACH: u64 = 8;
    pub const PROG_DETACH: u64 = 9;
    pub const OBJ_CLOSE: u64 = 11;
    pub const RAW_TRACEPOINT_OPEN: u64 = 17;
    pub const LINK_CREATE: u64 = 28;
    pub const ENABLE_STATS: u64 = 32;
}

#[allow(dead_code)]
mod bpf_error {
    use ax_errno::AxError;

    pub const EPERM: AxError = AxError::PermissionDenied;
    pub const ENOENT: AxError = AxError::NotFound;
    pub const ENOMEM: AxError = AxError::NoMemory;
    pub const EINVAL: AxError = AxError::InvalidInput;
    pub const ENOSPC: AxError = AxError::StorageFull;

    pub fn from_linux_errno(code: i32) -> AxError {
        match code {
            1 => AxError::PermissionDenied,
            2 => AxError::NotFound,
            12 => AxError::NoMemory,
            22 => AxError::InvalidInput,
            28 => AxError::StorageFull,
            _ => AxError::Io,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct BpfMapMeta {
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    map_flags: u32,
    id: u32,
}

trait BpfMapOps: Send + Sync {
    fn meta(&self) -> &BpfMapMeta;
    fn lookup_elem(&mut self, key: &[u8]) -> AxResult<Option<Vec<u8>>>;
    fn update_elem(&mut self, key: &[u8], value: &[u8], flags: u64) -> AxResult<()>;
    fn delete_elem(&mut self, key: &[u8]) -> AxResult<()>;
    fn get_next_key(&mut self, key: Option<&[u8]>) -> AxResult<Option<Vec<u8>>>;
}

struct ArrayMap {
    meta: BpfMapMeta,
    data: Vec<u8>,
    elem_size: usize,
}

impl ArrayMap {
    fn new(meta: BpfMapMeta) -> Self {
        let elem_size = meta.value_size as usize;
        let total = elem_size * meta.max_entries as usize;
        Self {
            meta,
            data: alloc::vec![0u8; total],
            elem_size,
        }
    }

    fn index_valid(&self, idx: u32) -> bool {
        (idx as usize) < self.meta.max_entries as usize
    }

    fn value_offset(&self, idx: u32) -> usize {
        idx as usize * self.elem_size
    }
}

impl BpfMapOps for ArrayMap {
    fn meta(&self) -> &BpfMapMeta {
        &self.meta
    }

    fn lookup_elem(&mut self, key: &[u8]) -> AxResult<Option<Vec<u8>>> {
        if key.len() != 4 {
            return Err(bpf_error::EINVAL);
        }
        let idx = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]);
        if !self.index_valid(idx) {
            return Err(bpf_error::ENOENT);
        }
        let start = self.value_offset(idx);
        let end = start + self.elem_size;
        Ok(Some(self.data[start..end].to_vec()))
    }

    fn update_elem(&mut self, key: &[u8], value: &[u8], _flags: u64) -> AxResult<()> {
        if key.len() != 4 {
            return Err(bpf_error::EINVAL);
        }
        if value.len() != self.elem_size {
            return Err(bpf_error::EINVAL);
        }
        let idx = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]);
        if !self.index_valid(idx) {
            return Err(bpf_error::ENOENT);
        }
        let start = self.value_offset(idx);
        self.data[start..start + self.elem_size].copy_from_slice(value);
        Ok(())
    }

    fn delete_elem(&mut self, _key: &[u8]) -> AxResult<()> {
        Err(bpf_error::EPERM)
    }

    fn get_next_key(&mut self, key: Option<&[u8]>) -> AxResult<Option<Vec<u8>>> {
        let next_idx = match key {
            None => 0u32,
            Some(k) => {
                if k.len() != 4 {
                    return Err(bpf_error::EINVAL);
                }
                let idx = u32::from_ne_bytes([k[0], k[1], k[2], k[3]]);
                idx + 1
            }
        };
        if next_idx >= self.meta.max_entries {
            return Ok(None);
        }
        Ok(Some(next_idx.to_ne_bytes().to_vec()))
    }
}

struct HashMapInner {
    meta: BpfMapMeta,
    entries: alloc::collections::BTreeMap<Vec<u8>, Vec<u8>>,
}

impl HashMapInner {
    fn new(meta: BpfMapMeta) -> Self {
        Self {
            meta,
            entries: alloc::collections::BTreeMap::new(),
        }
    }
}

impl BpfMapOps for HashMapInner {
    fn meta(&self) -> &BpfMapMeta {
        &self.meta
    }

    fn lookup_elem(&mut self, key: &[u8]) -> AxResult<Option<Vec<u8>>> {
        if key.len() != self.meta.key_size as usize {
            return Err(bpf_error::EINVAL);
        }
        Ok(self.entries.get(key).cloned())
    }

    fn update_elem(&mut self, key: &[u8], value: &[u8], flags: u64) -> AxResult<()> {
        if key.len() != self.meta.key_size as usize || value.len() != self.meta.value_size as usize
        {
            return Err(bpf_error::EINVAL);
        }
        let exists = self.entries.contains_key(key);
        const BPF_NOEXIST: u64 = 1;
        const BPF_EXISTS: u64 = 2;
        if flags & BPF_NOEXIST != 0 && exists {
            return Err(bpf_error::EPERM);
        }
        if flags & BPF_EXISTS != 0 && !exists {
            return Err(bpf_error::ENOENT);
        }
        if !exists && self.entries.len() >= self.meta.max_entries as usize {
            return Err(bpf_error::ENOMEM);
        }
        self.entries.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete_elem(&mut self, key: &[u8]) -> AxResult<()> {
        if key.len() != self.meta.key_size as usize {
            return Err(bpf_error::EINVAL);
        }
        if self.entries.remove(key).is_none() {
            return Err(bpf_error::ENOENT);
        }
        Ok(())
    }

    fn get_next_key(&mut self, key: Option<&[u8]>) -> AxResult<Option<Vec<u8>>> {
        match key {
            None => Ok(self.entries.keys().next().cloned()),
            Some(k) => {
                if k.len() != self.meta.key_size as usize {
                    return Err(bpf_error::EINVAL);
                }
                let mut found = false;
                for existing_key in self.entries.keys() {
                    if found {
                        return Ok(Some(existing_key.clone()));
                    }
                    if existing_key.as_slice() == k {
                        found = true;
                    }
                }
                Ok(None)
            }
        }
    }
}

struct PerfEventArrayMap {
    meta: BpfMapMeta,
    fds: alloc::vec::Vec<u32>,
    max_entries: u32,
}

impl PerfEventArrayMap {
    fn new(meta: BpfMapMeta) -> Self {
        let max_entries = meta.max_entries;
        let fds = alloc::vec![0u32; max_entries as usize];
        Self {
            meta,
            fds,
            max_entries,
        }
    }
}

impl BpfMapOps for PerfEventArrayMap {
    fn meta(&self) -> &BpfMapMeta {
        &self.meta
    }

    fn lookup_elem(&mut self, key: &[u8]) -> AxResult<Option<Vec<u8>>> {
        if key.len() != 4 {
            return Err(bpf_error::EINVAL);
        }
        let idx = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]);
        if idx as usize >= self.max_entries as usize {
            return Err(bpf_error::ENOENT);
        }
        Ok(Some(self.fds[idx as usize].to_ne_bytes().to_vec()))
    }

    fn update_elem(&mut self, key: &[u8], value: &[u8], _flags: u64) -> AxResult<()> {
        if key.len() != 4 || value.len() != 4 {
            return Err(bpf_error::EINVAL);
        }
        let idx = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]);
        if idx as usize >= self.max_entries as usize {
            return Err(bpf_error::ENOENT);
        }
        let fd = u32::from_ne_bytes([value[0], value[1], value[2], value[3]]);
        self.fds[idx as usize] = fd;
        Ok(())
    }

    fn delete_elem(&mut self, key: &[u8]) -> AxResult<()> {
        if key.len() != 4 {
            return Err(bpf_error::EINVAL);
        }
        let idx = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]);
        if idx as usize >= self.max_entries as usize {
            return Err(bpf_error::ENOENT);
        }
        self.fds[idx as usize] = 0;
        Ok(())
    }

    fn get_next_key(&mut self, key: Option<&[u8]>) -> AxResult<Option<Vec<u8>>> {
        let next_idx = match key {
            None => 0u32,
            Some(k) => {
                if k.len() != 4 {
                    return Err(bpf_error::EINVAL);
                }
                let idx = u32::from_ne_bytes([k[0], k[1], k[2], k[3]]);
                idx + 1
            }
        };
        if next_idx >= self.max_entries {
            return Ok(None);
        }
        Ok(Some(next_idx.to_ne_bytes().to_vec()))
    }
}

struct UnifiedMap {
    inner: alloc::boxed::Box<dyn BpfMapOps>,
}

impl core::fmt::Debug for UnifiedMap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnifiedMap").finish_non_exhaustive()
    }
}

impl UnifiedMap {
    fn new(map_type: u32, meta: BpfMapMeta) -> AxResult<Self> {
        let inner: alloc::boxed::Box<dyn BpfMapOps> = match map_type {
            map_type::ARRAY => alloc::boxed::Box::new(ArrayMap::new(meta.clone())),
            map_type::HASH => alloc::boxed::Box::new(HashMapInner::new(meta.clone())),
            map_type::PERF_EVENT_ARRAY => {
                alloc::boxed::Box::new(PerfEventArrayMap::new(meta.clone()))
            }
            _ => {
                warn!("bpf: unsupported map type {map_type}");
                return Err(bpf_error::EINVAL);
            }
        };
        Ok(Self { inner })
    }

    fn lookup(&mut self, key: &[u8]) -> AxResult<Option<Vec<u8>>> {
        self.inner.lookup_elem(key)
    }

    fn update(&mut self, key: &[u8], value: &[u8], flags: u64) -> AxResult<()> {
        self.inner.update_elem(key, value, flags)
    }

    fn delete(&mut self, key: &[u8]) -> AxResult<()> {
        self.inner.delete_elem(key)
    }

    fn get_next_key(&mut self, key: Option<&[u8]>) -> AxResult<Option<Vec<u8>>> {
        self.inner.get_next_key(key)
    }

    fn meta(&self) -> &BpfMapMeta {
        self.inner.meta()
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct BpfProg {
    prog_type: u32,
    insns: Vec<bpf_insn::BpfInsn>,
    meta: BpfProgMeta,
    id: u32,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct BpfProgMeta {
    prog_type: u32,
    name: alloc::string::String,
    license: alloc::string::String,
    kern_version: u32,
    prog_flags: u32,
    expected_attach_type: u32,
}

#[derive(Debug)]
struct BpfFdTable {
    maps: alloc::collections::BTreeMap<u32, UnifiedMap>,
    progs: alloc::collections::BTreeMap<u32, BpfProg>,
    links: alloc::collections::BTreeMap<u32, (u32, u32)>,
    next_fd: u32,
    free_fds: alloc::vec::Vec<u32>,
}

impl BpfFdTable {
    const fn new() -> Self {
        Self {
            maps: alloc::collections::BTreeMap::new(),
            progs: alloc::collections::BTreeMap::new(),
            links: alloc::collections::BTreeMap::new(),
            next_fd: 3,
            free_fds: alloc::vec::Vec::new(),
        }
    }

    fn alloc_fd(&mut self) -> u32 {
        if let Some(fd) = self.free_fds.pop() {
            fd
        } else {
            let fd = self.next_fd;
            self.next_fd += 1;
            fd
        }
    }

    fn insert_map(&mut self, map: UnifiedMap) -> u32 {
        let fd = self.alloc_fd();
        self.maps.insert(fd, map);
        fd
    }

    fn get_map(&mut self, fd: u32) -> AxResult<&mut UnifiedMap> {
        self.maps.get_mut(&fd).ok_or(AxError::BadFileDescriptor)
    }

    fn insert_prog(&mut self, prog: BpfProg) -> u32 {
        let fd = self.alloc_fd();
        self.progs.insert(fd, prog);
        fd
    }

    #[allow(dead_code)]
    fn get_prog(&mut self, fd: u32) -> AxResult<&mut BpfProg> {
        self.progs.get_mut(&fd).ok_or(AxError::BadFileDescriptor)
    }

    fn remove_map(&mut self, fd: u32) -> AxResult<()> {
        self.maps
            .remove(&fd)
            .map(|_| ())
            .ok_or(AxError::BadFileDescriptor)
    }

    fn remove_prog(&mut self, fd: u32) -> AxResult<()> {
        self.progs
            .remove(&fd)
            .map(|_| ())
            .ok_or(AxError::BadFileDescriptor)
    }

    fn close_fd(&mut self, fd: u32) -> AxResult<()> {
        let result = if self.maps.contains_key(&fd) {
            self.remove_map(fd)
        } else if self.progs.contains_key(&fd) {
            self.remove_prog(fd)
        } else if self.links.contains_key(&fd) {
            self.links
                .remove(&fd)
                .map(|_| ())
                .ok_or(AxError::BadFileDescriptor)
        } else {
            Err(AxError::BadFileDescriptor)
        };
        if result.is_ok() {
            self.free_fds.push(fd);
        }
        result
    }

    #[allow(dead_code)]
    fn fd_exists(&self, fd: u32) -> bool {
        self.maps.contains_key(&fd) || self.progs.contains_key(&fd) || self.links.contains_key(&fd)
    }
}

static BPF_GLOBAL: SpinNoIrq<BpfFdTable> = SpinNoIrq::new(BpfFdTable::new());
#[ax_percpu::def_percpu]
static BPF_LOOKUP_CACHE: alloc::vec::Vec<u8> = alloc::vec::Vec::new();

fn handle_map_create(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 24 {
        return Err(bpf_error::EINVAL);
    }
    let map_type;
    let key_size;
    let value_size;
    let max_entries;
    let map_flags;
    unsafe {
        let ptr = uattr as *const u32;
        map_type = core::ptr::read(ptr);
        key_size = core::ptr::read(ptr.add(1));
        value_size = core::ptr::read(ptr.add(2));
        max_entries = core::ptr::read(ptr.add(3));
        map_flags = core::ptr::read(ptr.add(4));
    }
    if max_entries == 0 || key_size == 0 || value_size == 0 {
        return Err(bpf_error::EINVAL);
    }
    let mut guard = BPF_GLOBAL.lock();
    let id = guard.maps.len() as u32;
    let meta = BpfMapMeta {
        map_type,
        key_size,
        value_size,
        max_entries,
        map_flags,
        id,
    };
    let map = UnifiedMap::new(map_type, meta)?;
    let fd = guard.insert_map(map);
    info!(
        "bpf: created map type={map_type} key={key_size} val={value_size} max={max_entries} \
         fd={fd}"
    );
    Ok(fd as isize)
}

fn handle_map_lookup_elem(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 20 {
        return Err(bpf_error::EINVAL);
    }
    let (map_fd, key_ptr, value_ptr) = unsafe {
        let ptr = uattr as *const u64;
        let map_fd = core::ptr::read(ptr) as u32;
        let key_ptr = core::ptr::read(ptr.add(1)) as usize;
        let value_ptr = core::ptr::read(ptr.add(2)) as usize;
        (map_fd, key_ptr, value_ptr)
    };
    let mut guard = BPF_GLOBAL.lock();
    let map = guard.get_map(map_fd)?;
    let key_size = map.meta().key_size as usize;
    let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
    match map.lookup(key)? {
        Some(val) => {
            unsafe {
                core::ptr::copy_nonoverlapping(val.as_ptr(), value_ptr as *mut u8, val.len());
            }
            Ok(0)
        }
        None => Err(bpf_error::ENOENT),
    }
}

fn handle_map_update_elem(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 28 {
        return Err(bpf_error::EINVAL);
    }
    let (map_fd, key_ptr, value_ptr, flags) = unsafe {
        let ptr = uattr as *const u64;
        let map_fd = core::ptr::read(ptr) as u32;
        let key_ptr = core::ptr::read(ptr.add(1)) as usize;
        let value_ptr = core::ptr::read(ptr.add(2)) as usize;
        let flags = core::ptr::read(ptr.add(3));
        (map_fd, key_ptr, value_ptr, flags)
    };
    let mut guard = BPF_GLOBAL.lock();
    let map = guard.get_map(map_fd)?;
    let key_size = map.meta().key_size as usize;
    let value_size = map.meta().value_size as usize;
    let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
    let value = unsafe { core::slice::from_raw_parts(value_ptr as *const u8, value_size) };
    map.update(key, value, flags)?;
    Ok(0)
}

fn handle_map_delete_elem(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 12 {
        return Err(bpf_error::EINVAL);
    }
    let (map_fd, key_ptr) = unsafe {
        let ptr = uattr as *const u64;
        let map_fd = core::ptr::read(ptr) as u32;
        let key_ptr = core::ptr::read(ptr.add(1)) as usize;
        (map_fd, key_ptr)
    };
    let mut guard = BPF_GLOBAL.lock();
    let map = guard.get_map(map_fd)?;
    let key_size = map.meta().key_size as usize;
    let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
    map.delete(key)?;
    Ok(0)
}

fn handle_map_get_next_key(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 28 {
        return Err(bpf_error::EINVAL);
    }
    let (map_fd, key_ptr, next_key_ptr) = unsafe {
        let ptr = uattr as *const u64;
        let map_fd = core::ptr::read(ptr) as u32;
        let key_ptr = core::ptr::read(ptr.add(1)) as usize;
        let next_key_ptr = core::ptr::read(ptr.add(2)) as usize;
        (map_fd, key_ptr, next_key_ptr)
    };
    let mut guard = BPF_GLOBAL.lock();
    let map = guard.get_map(map_fd)?;
    let key_size = map.meta().key_size as usize;
    let key_opt = if key_ptr != 0 {
        Some(unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) })
    } else {
        None
    };
    match map.get_next_key(key_opt)? {
        Some(next_key) => {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    next_key.as_ptr(),
                    next_key_ptr as *mut u8,
                    key_size,
                );
            }
            Ok(0)
        }
        None => Err(bpf_error::ENOENT),
    }
}

fn handle_prog_load(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 48 {
        return Err(bpf_error::EINVAL);
    }
    let (
        prog_type,
        insn_cnt,
        insns_ptr,
        _license_ptr,
        log_level,
        _log_size,
        _log_buf,
        kern_version,
        prog_flags,
    ) = unsafe {
        let read_u32 = |off: usize| core::ptr::read((uattr + off) as *const u32);
        let read_u64 = |off: usize| core::ptr::read((uattr + off) as *const u64);
        let prog_type = read_u32(0);
        let insn_cnt = read_u32(4);
        let insns_ptr = read_u64(8);
        let license_ptr = read_u64(16);
        let log_level = read_u32(24);
        let log_size = read_u32(28);
        let log_buf = read_u64(32);
        let kern_version = read_u32(40);
        let prog_flags = read_u32(44);
        (
            prog_type,
            insn_cnt,
            insns_ptr,
            license_ptr,
            log_level,
            log_size,
            log_buf,
            kern_version,
            prog_flags,
        )
    };
    if log_level > 0 {
        warn!("bpf: BPF_PROG_LOAD verifier log requested but not implemented");
    }
    match prog_type {
        prog_type::KPROBE
        | prog_type::TRACEPOINT
        | prog_type::RAW_TRACEPOINT
        | prog_type::PERF_EVENT
        | prog_type::UNSPEC => {}
        _ => {
            warn!("bpf: unsupported prog type {prog_type}");
            return Err(bpf_error::EINVAL);
        }
    }
    let insn_bytes = insn_cnt as usize * 8;
    let raw_insns = unsafe { core::slice::from_raw_parts(insns_ptr as *const u8, insn_bytes) };
    let mut insns = Vec::new();
    for chunk in raw_insns.chunks_exact(8) {
        let arr: [u8; 8] = chunk.try_into().unwrap();
        insns.push(bpf_insn::BpfInsn::from_bytes(&arr));
    }
    if let Err(e) = BpfVm::verify_program(&insns) {
        warn!("bpf: program verification failed: {e}");
        return Err(bpf_error::EINVAL);
    }
    let mut guard = BPF_GLOBAL.lock();
    let id = guard.progs.len() as u32;
    let prog = BpfProg {
        prog_type,
        insns,
        meta: BpfProgMeta {
            prog_type,
            name: alloc::format!("prog_{id}"),
            license: alloc::string::String::new(),
            kern_version,
            prog_flags,
            expected_attach_type: 0,
        },
        id,
    };
    let fd = guard.insert_prog(prog);
    info!("bpf: loaded prog type={prog_type} insns={insn_cnt} fd={fd}");
    Ok(fd as isize)
}

fn handle_raw_tracepoint_open(_uattr: usize, _size: u32) -> AxResult<isize> {
    warn!("bpf: BPF_RAW_TRACEPOINT_OPEN not yet implemented");
    Err(bpf_error::EINVAL)
}

#[allow(dead_code)]
mod helper_id {
    pub const MAP_LOOKUP_ELEM: u32 = 1;
    pub const MAP_UPDATE_ELEM: u32 = 2;
    pub const MAP_DELETE_ELEM: u32 = 3;
    pub const PROBE_READ: u32 = 4;
    pub const KTIME_GET_NS: u32 = 5;
    pub const TRACE_PRINTK: u32 = 6;
    pub const GET_PRANDOM_U32: u32 = 7;
    pub const GET_SMP_PROCESSOR_ID: u32 = 8;
    pub const SKB_STORE_BYTES: u32 = 9;
    pub const CSUM_DIFF: u32 = 10;
    pub const GET_CURRENT_PID_TGID: u32 = 14;
    pub const GET_CURRENT_UID_GID: u32 = 15;
    pub const GET_CURRENT_COMM: u32 = 16;
    pub const PERF_EVENT_OUTPUT: u32 = 25;
    pub const GET_STACK_ID: u32 = 27;
    pub const GET_CURRENT_CGROUP_ID: u32 = 43;
    pub const PROBE_READ_USER: u32 = 112;
    pub const PROBE_READ_KERNEL: u32 = 113;
    pub const PROBE_READ_USER_STR: u32 = 114;
    pub const PROBE_READ_KERNEL_STR: u32 = 115;
    pub const MAP_PUSH_ELEM: u32 = 87;
    pub const MAP_POP_ELEM: u32 = 88;
    pub const MAP_PEEK_ELEM: u32 = 89;
    pub const RINGBUF_OUTPUT: u32 = 130;
    pub const RINGBUF_RESERVE: u32 = 131;
    pub const RINGBUF_SUBMIT: u32 = 132;
    pub const RINGBUF_DISCARD: u32 = 133;
    pub const GET_CURRENT_TASK: u32 = 35;
    pub const MAP_FOR_EACH_ELEM: u32 = 164;
    pub const GET_ATTACHED_FUNC_ARGS: u32 = 186;
}

type HelperFn = fn(u64, u64, u64, u64, u64) -> u64;

fn init_helper_functions() -> alloc::collections::BTreeMap<u32, HelperFn> {
    let mut m: alloc::collections::BTreeMap<u32, HelperFn> = alloc::collections::BTreeMap::new();
    m.insert(helper_id::MAP_LOOKUP_ELEM, helper_map_lookup_elem);
    m.insert(helper_id::MAP_UPDATE_ELEM, helper_map_update_elem);
    m.insert(helper_id::MAP_DELETE_ELEM, helper_map_delete_elem);
    m.insert(helper_id::PROBE_READ, helper_probe_read);
    m.insert(helper_id::PROBE_READ_KERNEL, helper_probe_read);
    m.insert(helper_id::KTIME_GET_NS, helper_ktime_get_ns);
    m.insert(helper_id::GET_SMP_PROCESSOR_ID, helper_get_smp_processor_id);
    m.insert(helper_id::GET_CURRENT_PID_TGID, helper_get_current_pid_tgid);
    m.insert(helper_id::GET_CURRENT_UID_GID, helper_get_current_uid_gid);
    m.insert(helper_id::GET_PRANDOM_U32, helper_get_prandom_u32);
    m.insert(helper_id::PERF_EVENT_OUTPUT, helper_perf_event_output);
    m.insert(helper_id::TRACE_PRINTK, helper_trace_printk);
    m.insert(helper_id::GET_CURRENT_TASK, helper_get_current_task);
    m.insert(helper_id::GET_CURRENT_COMM, helper_get_current_comm);
    m
}

fn helper_map_lookup_elem(map_ptr: u64, key_ptr: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    if map_ptr == 0 || key_ptr == 0 {
        return 0;
    }
    let mut guard = BPF_GLOBAL.lock();
    let map = match guard.get_map(map_ptr as u32) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let key_size = map.meta().key_size as usize;
    let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
    match map.lookup(key) {
        Ok(Some(value)) => BPF_LOOKUP_CACHE.with_current(|cache| {
            cache.clear();
            cache.extend_from_slice(&value);
            cache.as_ptr() as u64
        }),
        _ => 0,
    }
}

fn helper_map_update_elem(map_ptr: u64, key_ptr: u64, value_ptr: u64, flags: u64, _a5: u64) -> u64 {
    if map_ptr == 0 || key_ptr == 0 || value_ptr == 0 {
        return u64::MAX;
    }
    let mut guard = BPF_GLOBAL.lock();
    let map = match guard.get_map(map_ptr as u32) {
        Ok(m) => m,
        Err(_) => return u64::MAX,
    };
    let key_size = map.meta().key_size as usize;
    let value_size = map.meta().value_size as usize;
    let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
    let value = unsafe { core::slice::from_raw_parts(value_ptr as *const u8, value_size) };
    match map.update(key, value, flags) {
        Ok(()) => 0,
        Err(_) => u64::MAX,
    }
}

fn helper_map_delete_elem(map_ptr: u64, key_ptr: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    if map_ptr == 0 || key_ptr == 0 {
        return u64::MAX;
    }
    let mut guard = BPF_GLOBAL.lock();
    let map = match guard.get_map(map_ptr as u32) {
        Ok(m) => m,
        Err(_) => return u64::MAX,
    };
    let key_size = map.meta().key_size as usize;
    let key = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_size) };
    match map.delete(key) {
        Ok(()) => 0,
        Err(_) => u64::MAX,
    }
}

fn helper_probe_read(dst: u64, size: u64, src: u64, _a4: u64, _a5: u64) -> u64 {
    if dst == 0 || size == 0 {
        return u64::MAX;
    }
    let len = size as usize;
    if len > 4096 {
        return u64::MAX;
    }
    if src == 0 {
        unsafe { core::ptr::write_bytes(dst as *mut u8, 0, len) };
        return 0;
    }
    let src_slice = unsafe { core::slice::from_raw_parts(src as *const u8, len) };
    let dst_slice = unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, len) };
    let copied = {
        let buf = unsafe {
            core::slice::from_raw_parts_mut(
                dst_slice.as_mut_ptr() as *mut core::mem::MaybeUninit<u8>,
                len,
            )
        };
        match starry_vm::vm_read_slice(src as *const u8, buf) {
            Ok(()) => len,
            Err(_) => {
                unsafe {
                    core::ptr::copy_nonoverlapping(src_slice.as_ptr(), dst_slice.as_mut_ptr(), len)
                };
                len
            }
        }
    };
    let _ = copied;
    0
}

fn helper_ktime_get_ns(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

fn helper_get_smp_processor_id(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    ax_runtime::hal::percpu::this_cpu_id() as u64
}

fn helper_get_current_pid_tgid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    let curr = ax_task::current();
    let pid = curr.id().as_u64();
    let tgid = pid;
    (tgid << 32) | pid
}

fn helper_get_current_uid_gid(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    let curr = ax_task::current();
    let cred = curr.as_thread().cred();
    let uid = cred.uid as u64;
    let gid = cred.gid as u64;
    (gid << 32) | uid
}

fn helper_get_prandom_u32(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    use core::sync::atomic::{AtomicU32, Ordering};
    static SEED: AtomicU32 = AtomicU32::new(12345);
    let prev = SEED.load(Ordering::Relaxed);
    let next = prev.wrapping_mul(1103515245).wrapping_add(12345);
    SEED.store(next, Ordering::Relaxed);
    next as u64
}

fn helper_perf_event_output(
    _ctx: u64,
    map_fd: u64,
    _flags: u64,
    data_ptr: u64,
    data_size: u64,
) -> u64 {
    if data_ptr == 0 || data_size == 0 {
        return u64::MAX;
    }
    let data = unsafe { core::slice::from_raw_parts(data_ptr as *const u8, data_size as usize) };
    match crate::perf_event::perf_event_write(map_fd as u32, data) {
        Ok(()) => 0,
        Err(_) => u64::MAX,
    }
}

fn helper_trace_printk(fmt_ptr: u64, fmt_size: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    if fmt_ptr == 0 || fmt_size == 0 || fmt_size > 128 {
        return u64::MAX;
    }
    let len = fmt_size as usize;
    let bytes = unsafe { core::slice::from_raw_parts(fmt_ptr as *const u8, len) };
    let s = core::str::from_utf8(bytes).unwrap_or("<invalid utf8>");
    let trimmed = s.trim_end_matches('\0');
    if !trimmed.is_empty() {
        warn!("bpf trace_printk: {trimmed}");
    }
    len as u64
}

fn helper_get_current_task(_a1: u64, _a2: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    let curr = ax_task::current();
    curr.as_ref() as *const _ as u64
}

fn helper_get_current_comm(buf: u64, size: u64, _a3: u64, _a4: u64, _a5: u64) -> u64 {
    if buf == 0 || size == 0 {
        return u64::MAX;
    }
    let curr = ax_task::current();
    let name = curr.name();
    let copy_len = core::cmp::min(name.len(), size as usize - 1);
    unsafe {
        let dst = core::slice::from_raw_parts_mut(buf as *mut u8, copy_len);
        dst.copy_from_slice(name.as_bytes());
        if copy_len < size as usize {
            *dst.get_unchecked_mut(copy_len) = 0;
        }
    }
    0
}

const BPF_MAX_INSN: usize = 1000000;
const BPF_MAX_STACK: usize = 512;

struct BpfVm {
    #[allow(dead_code)]
    helpers: alloc::collections::BTreeMap<u32, HelperFn>,
}

impl BpfVm {
    fn new() -> Self {
        Self {
            helpers: init_helper_functions(),
        }
    }

    fn verify_program(insns: &[bpf_insn::BpfInsn]) -> Result<(), &'static str> {
        if insns.len() > BPF_MAX_INSN {
            warn!("bpf verifier: program too large: {} insns", insns.len());
            return Err("program too large");
        }
        let has_exit = insns.iter().any(|insn| {
            let class = insn.class();
            class == bpf_insn::BPF_JMP && (insn.code & 0xf0) == bpf_insn::BPF_EXIT
        });
        if !has_exit {
            warn!("bpf verifier: program has no BPF_EXIT instruction");
            return Err("no BPF_EXIT instruction");
        }
        for (pc, insn) in insns.iter().enumerate() {
            let class = insn.class();
            if class == bpf_insn::BPF_JMP || class == bpf_insn::BPF_JMP32 {
                let op = insn.code & 0xf0;
                if op != bpf_insn::BPF_EXIT && op != 0x80 {
                    let target = pc as isize + 1 + insn.off as isize;
                    if target < 0 || target as usize >= insns.len() {
                        warn!("bpf verifier: jump out of bounds at pc={pc} target={target}");
                        return Err("jump out of bounds");
                    }
                }
            }
        }
        Ok(())
    }

    fn execute(
        &self,
        insns: &[bpf_insn::BpfInsn],
        ctx: u64,
        ctx_size: u32,
    ) -> Result<u64, &'static str> {
        if insns.is_empty() {
            return Err("empty program");
        }
        let mut regs = [0u64; 11];
        regs[1] = ctx;
        let mut stack = [0u8; BPF_MAX_STACK];
        regs[10] = stack.as_mut_ptr() as u64 + BPF_MAX_STACK as u64;
        let mut pc: usize = 0;
        let max_pc = insns.len();
        for _ in 0..BPF_MAX_INSN {
            if pc >= max_pc {
                return Err("PC out of bounds");
            }
            let insn = &insns[pc];
            let class = insn.class();
            match class {
                bpf_insn::BPF_ALU | bpf_insn::BPF_ALU64 => {
                    let is_64 = class == bpf_insn::BPF_ALU64;
                    let dst = insn.dst_reg() as usize;
                    let src_val = if insn.code & bpf_insn::BPF_X != 0 {
                        regs[insn.src_reg() as usize]
                    } else {
                        insn.imm as u64
                    };
                    let result = Self::exec_alu(insn.alu_op(), regs[dst], src_val, is_64);
                    regs[dst] = result;
                    pc += 1;
                }
                bpf_insn::BPF_JMP | bpf_insn::BPF_JMP32 => {
                    let op = insn.code & 0xf0;
                    if op == bpf_insn::BPF_EXIT {
                        return Ok(regs[0]);
                    }
                    if op == bpf_insn::BPF_CALL_OP {
                        let helper_id = insn.imm as u32;
                        if let Some(helper_fn) = self.helpers.get(&helper_id) {
                            regs[0] = helper_fn(regs[1], regs[2], regs[3], regs[4], regs[5]);
                        } else {
                            warn!("bpf: unknown helper {}", helper_id);
                            regs[0] = u64::MAX;
                        }
                        pc += 1;
                        continue;
                    }
                    let is_64 = class == bpf_insn::BPF_JMP;
                    let dst = insn.dst_reg() as usize;
                    let src_val = if insn.code & bpf_insn::BPF_X != 0 {
                        regs[insn.src_reg() as usize]
                    } else {
                        insn.imm as u64
                    };
                    let dst_val = regs[dst];
                    let off = insn.off as isize;
                    if insn.code == (bpf_insn::BPF_JMP | bpf_insn::BPF_JA) {
                        pc = (pc as isize + 1 + off) as usize;
                        continue;
                    }
                    if insn.code == (bpf_insn::BPF_JMP32 | bpf_insn::BPF_JA) {
                        pc = (pc as isize + 1 + off) as usize;
                        continue;
                    }
                    if Self::eval_jmp(insn.code, dst_val, src_val, is_64) {
                        pc = (pc as isize + 1 + off) as usize;
                    } else {
                        pc += 1;
                    }
                }
                bpf_insn::BPF_ST | bpf_insn::BPF_STX => {
                    Self::exec_store(insn, &mut regs, &mut stack);
                    pc += 1;
                }
                bpf_insn::BPF_LDX => {
                    if !Self::exec_pkt_load(insn, &mut regs, ctx, ctx_size) {
                        Self::exec_load(insn, &mut regs, &stack);
                    }
                    pc += 1;
                }
                bpf_insn::BPF_LD => {
                    if insn.is_ld_dw_imm() && pc + 1 < max_pc {
                        let next = &insns[pc + 1];
                        let imm_lo = insn.imm as u64;
                        let imm_hi = next.imm as u64;
                        let val = (imm_hi << 32) | (imm_lo & 0xffffffff);
                        regs[insn.dst_reg() as usize] = val;
                        pc += 2;
                    } else if Self::exec_pkt_load(insn, &mut regs, ctx, ctx_size) {
                        pc += 1;
                    } else {
                        return Err("unsupported LD instruction");
                    }
                }
                _ => return Err("unsupported instruction class"),
            }
        }
        Err("max instructions exceeded")
    }

    /// Handle BPF_ABS / BPF_IND / BPF_LEN / BPF_MSH loads from the
    /// context buffer.  Returns `true` if the instruction was consumed.
    fn exec_pkt_load(
        insn: &bpf_insn::BpfInsn,
        regs: &mut [u64; 11],
        ctx: u64,
        ctx_size: u32,
    ) -> bool {
        let mode = insn.mode();
        match mode {
            bpf_insn::BPF_LEN => {
                let dst = insn.dst_reg() as usize;
                regs[dst] = ctx_size as u64;
                true
            }
            bpf_insn::BPF_MSH => {
                if ctx_size == 0 || insn.imm as u32 >= ctx_size {
                    return false;
                }
                let byte = unsafe { *((ctx as usize + insn.imm as usize) as *const u8) };
                let dst = insn.dst_reg() as usize;
                regs[dst] = 4u64 * (byte as u64 & 0xf);
                true
            }
            bpf_insn::BPF_ABS | bpf_insn::BPF_IND => {
                if ctx_size == 0 {
                    return false;
                }
                let base_off = if mode == bpf_insn::BPF_IND {
                    regs[6].wrapping_add(insn.imm as u64) as usize
                } else {
                    insn.imm as usize
                };

                let size = insn.size();
                let val: u64 = match size {
                    bpf_insn::BPF_W => {
                        if base_off + 4 > ctx_size as usize {
                            return false;
                        }
                        unsafe { *((ctx as usize + base_off) as *const u32) as u64 }
                    }
                    bpf_insn::BPF_H => {
                        if base_off + 2 > ctx_size as usize {
                            return false;
                        }
                        unsafe { *((ctx as usize + base_off) as *const u16) as u64 }
                    }
                    bpf_insn::BPF_B => {
                        if base_off + 1 > ctx_size as usize {
                            return false;
                        }
                        unsafe { *((ctx as usize + base_off) as *const u8) as u64 }
                    }
                    bpf_insn::BPF_DW => {
                        if base_off + 8 > ctx_size as usize {
                            return false;
                        }
                        unsafe { *((ctx as usize + base_off) as *const u64) }
                    }
                    _ => return false,
                };
                let dst = insn.dst_reg() as usize;
                regs[dst] = val;
                true
            }
            _ => false,
        }
    }

    fn exec_alu(op: u8, dst: u64, src: u64, is_64: bool) -> u64 {
        let mask = if is_64 { !0u64 } else { 0xffffffff };
        let result = match op {
            bpf_insn::BPF_ADD => dst.wrapping_add(src),
            bpf_insn::BPF_SUB => dst.wrapping_sub(src),
            bpf_insn::BPF_MUL => dst.wrapping_mul(src),
            bpf_insn::BPF_DIV => {
                if src == 0 {
                    return 0;
                }
                dst / src
            }
            bpf_insn::BPF_OR => dst | src,
            bpf_insn::BPF_AND => dst & src,
            bpf_insn::BPF_LSH => dst.wrapping_shl(src as u32),
            bpf_insn::BPF_RSH => {
                if is_64 {
                    dst >> src
                } else {
                    (dst as u32 >> src as u32) as u64
                }
            }
            bpf_insn::BPF_NEG => (-(dst as i64)) as u64,
            bpf_insn::BPF_MOD => {
                if src == 0 {
                    return dst & mask;
                }
                dst % src
            }
            bpf_insn::BPF_XOR => dst ^ src,
            bpf_insn::BPF_MOV => src,
            bpf_insn::BPF_ARSH => {
                if is_64 {
                    ((dst as i64) >> src) as u64
                } else {
                    (((dst as i32) as i64) >> src) as u64
                }
            }
            _ => return dst,
        };
        result & mask
    }

    fn eval_jmp(code: u8, dst: u64, src: u64, is_64: bool) -> bool {
        let op = code & 0xf0;
        let (d, s) = if is_64 {
            (dst, src)
        } else {
            (dst as u32 as u64, src as u32 as u64)
        };
        match op {
            bpf_insn::BPF_JEQ => d == s,
            bpf_insn::BPF_JGT => d > s,
            bpf_insn::BPF_JGE => d >= s,
            bpf_insn::BPF_JSET => (d & s) != 0,
            bpf_insn::BPF_JNE => d != s,
            bpf_insn::BPF_JSGT => (d as i64) > (s as i64),
            bpf_insn::BPF_JSGE => (d as i64) >= (s as i64),
            bpf_insn::BPF_JLT => d < s,
            bpf_insn::BPF_JLE => d <= s,
            bpf_insn::BPF_JSLT => (d as i64) < (s as i64),
            bpf_insn::BPF_JSLE => (d as i64) <= (s as i64),
            _ => false,
        }
    }

    #[allow(clippy::comparison_chain)]
    fn exec_store(insn: &bpf_insn::BpfInsn, regs: &mut [u64; 11], stack: &mut [u8; BPF_MAX_STACK]) {
        let dst_base = regs[10];
        let off = insn.off as i32 as isize;
        let mem = insn.mode();
        if mem == bpf_insn::BPF_MEM {
            let addr = (dst_base as isize + off) as usize;
            let stack_base = stack.as_mut_ptr() as usize;
            if addr < stack_base || addr + 8 > stack_base + BPF_MAX_STACK {
                return;
            }
            let val = if insn.class() == bpf_insn::BPF_ST {
                insn.imm as u64
            } else {
                regs[insn.src_reg() as usize]
            };
            match insn.size() {
                bpf_insn::BPF_W => unsafe {
                    let p = (addr - stack_base) as *mut u32;
                    *p = val as u32;
                },
                bpf_insn::BPF_H => unsafe {
                    let p = (addr - stack_base) as *mut u16;
                    *p = val as u16;
                },
                bpf_insn::BPF_B => unsafe {
                    let p = (addr - stack_base) as *mut u8;
                    *p = val as u8;
                },
                bpf_insn::BPF_DW => unsafe {
                    let p = (addr - stack_base) as *mut u64;
                    *p = val;
                },
                _ => {}
            }
        }
    }

    fn exec_load(insn: &bpf_insn::BpfInsn, regs: &mut [u64; 11], stack: &[u8; BPF_MAX_STACK]) {
        let src_base = regs[insn.src_reg() as usize];
        let off = insn.off as i32 as isize;
        let mem = insn.mode();
        if mem == bpf_insn::BPF_MEM {
            let addr = (src_base as isize + off) as usize;
            let stack_base = stack.as_ptr() as usize;
            if addr < stack_base || addr + 8 > stack_base + BPF_MAX_STACK {
                return;
            }
            let val: u64 = match insn.size() {
                bpf_insn::BPF_W => unsafe {
                    let p = (addr - stack_base) as *const u32;
                    (*p) as u64
                },
                bpf_insn::BPF_H => unsafe {
                    let p = (addr - stack_base) as *const u16;
                    (*p) as u64
                },
                bpf_insn::BPF_B => unsafe {
                    let p = (addr - stack_base) as *const u8;
                    (*p) as u64
                },
                bpf_insn::BPF_DW => unsafe {
                    let p = (addr - stack_base) as *const u64;
                    *p
                },
                _ => 0,
            };
            regs[insn.dst_reg() as usize] = val;
        }
    }
}

pub fn run_bpf_prog(fd: u32, ctx: u64) -> AxResult<u64> {
    let (insns, prog_type) = {
        let guard = BPF_GLOBAL.lock();
        let prog = guard.progs.get(&fd).ok_or(AxError::BadFileDescriptor)?;
        (prog.insns.clone(), prog.prog_type)
    };
    let _ = prog_type;
    let vm = BpfVm::new();
    vm.execute(&insns, ctx, 0).map_err(|e| {
        warn!("bpf: program execution failed: {e}");
        AxError::Io
    })
}

/// Execute a seccomp BPF filter program directly (no fd table lookup).
///
/// The context pointer points to `SeccompData` and `ctx_size` bytes
/// are readable through BPF_ABS / BPF_IND.
///
/// Returns the filter's action value (one of the `SECCOMP_RET_*`
/// constants), or `SECCOMP_RET_ALLOW` on VM error.
pub fn run_seccomp_bpf(insns: &[bpf_insn::BpfInsn], ctx: u64, ctx_size: u32) -> u32 {
    use crate::syscall::sys::seccomp_ret;
    let vm = BpfVm::new();
    match vm.execute(insns, ctx, ctx_size) {
        Ok(v) => v as u32,
        Err(e) => {
            warn!("seccomp bpf: execution failed: {e}");
            seccomp_ret::ALLOW
        }
    }
}

fn handle_link_create(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 20 {
        return Err(bpf_error::EINVAL);
    }
    let (prog_fd, target_fd) = unsafe {
        let ptr = uattr as *const u32;
        let prog_fd = core::ptr::read(ptr) as u32;
        let target_fd = core::ptr::read(ptr.add(1)) as u32;
        let _attach_type = core::ptr::read(ptr.add(2));
        (prog_fd, target_fd)
    };
    crate::perf_event::perf_event_attach_prog(target_fd, prog_fd)?;
    crate::perf_event::perf_event_enable(target_fd)?;
    let link_fd = {
        let mut guard = BPF_GLOBAL.lock();
        let link_fd = guard.alloc_fd();
        guard.links.insert(link_fd, (prog_fd, target_fd));
        link_fd
    };
    info!("bpf: LINK_CREATE prog_fd={prog_fd} target_fd={target_fd} link_fd={link_fd}");
    Ok(link_fd as isize)
}

fn handle_obj_close(uattr: usize, size: u32) -> AxResult<isize> {
    if size < 4 {
        return Err(bpf_error::EINVAL);
    }
    let fd = unsafe { core::ptr::read(uattr as *const u32) };
    let mut guard = BPF_GLOBAL.lock();
    guard.close_fd(fd)?;
    info!("bpf: OBJ_CLOSE fd={fd}");
    Ok(0)
}

fn handle_prog_attach(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    if size < 16 {
        return Err(bpf_error::EINVAL);
    }
    let (target_fd, attach_prog_fd) = unsafe {
        let ptr = uattr as *const u32;
        let target_fd = core::ptr::read(ptr) as u32;
        let attach_prog_fd = core::ptr::read(ptr.add(1)) as u32;
        let _attach_type = core::ptr::read(ptr.add(2));
        (target_fd, attach_prog_fd)
    };
    if cmd == cmd::PROG_ATTACH {
        crate::perf_event::perf_event_attach_prog(target_fd, attach_prog_fd)?;
        crate::perf_event::perf_event_enable(target_fd)?;
        info!("bpf: PROG_ATTACH attach_prog_fd={attach_prog_fd} target_fd={target_fd}");
    } else {
        crate::perf_event::perf_event_disable(target_fd)?;
        info!("bpf: PROG_DETACH target_fd={target_fd}");
    }
    Ok(0)
}

pub fn sys_bpf(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    match cmd {
        cmd::MAP_CREATE => handle_map_create(uattr, size),
        cmd::PROG_LOAD => handle_prog_load(uattr, size),
        cmd::MAP_LOOKUP_ELEM => handle_map_lookup_elem(uattr, size),
        cmd::MAP_UPDATE_ELEM => handle_map_update_elem(uattr, size),
        cmd::MAP_DELETE_ELEM => handle_map_delete_elem(uattr, size),
        cmd::MAP_GET_NEXT_KEY => handle_map_get_next_key(uattr, size),
        cmd::RAW_TRACEPOINT_OPEN => handle_raw_tracepoint_open(uattr, size),
        cmd::OBJ_CLOSE => handle_obj_close(uattr, size),
        cmd::OBJ_PIN | cmd::OBJ_GET => {
            warn!("bpf: obj pin/get not yet implemented");
            Err(bpf_error::EINVAL)
        }
        cmd::PROG_ATTACH | cmd::PROG_DETACH => handle_prog_attach(cmd, uattr, size),
        cmd::LINK_CREATE => handle_link_create(uattr, size),
        cmd::ENABLE_STATS => {
            warn!("bpf: ENABLE_STATS not yet implemented");
            Err(bpf_error::EINVAL)
        }
        _ => {
            warn!("bpf: unknown command {cmd}");
            Err(bpf_error::EINVAL)
        }
    }
}

pub fn sys_perf_event_open(
    attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> AxResult<isize> {
    crate::perf_event::sys_perf_event_open_impl(attr_uptr, pid, cpu, group_fd, flags)
}

pub fn bpf_close_fd(fd: u32) -> AxResult<()> {
    let mut guard = BPF_GLOBAL.lock();
    guard.close_fd(fd)
}

pub fn bpf_close_all_fds() {
    let mut guard = BPF_GLOBAL.lock();
    guard.maps.clear();
    guard.progs.clear();
    guard.links.clear();
    guard.free_fds.clear();
}

#[allow(dead_code)]
pub fn bpf_fd_exists(fd: u32) -> bool {
    let guard = BPF_GLOBAL.lock();
    guard.fd_exists(fd)
}
