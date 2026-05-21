use alloc::vec::Vec;

use ax_errno::{AxError, AxResult};
use ax_sync::spin::SpinNoIrq;

#[allow(dead_code)]
mod bpf_insn {
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
    next_fd: u32,
}

impl BpfFdTable {
    const fn new() -> Self {
        Self {
            maps: alloc::collections::BTreeMap::new(),
            progs: alloc::collections::BTreeMap::new(),
            next_fd: 3,
        }
    }

    fn alloc_fd(&mut self) -> u32 {
        let fd = self.next_fd;
        self.next_fd += 1;
        fd
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
}

static BPF_GLOBAL: SpinNoIrq<BpfFdTable> = SpinNoIrq::new(BpfFdTable::new());

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
        let ptr = uattr as *const u32;
        let prog_type = core::ptr::read(ptr);
        let insn_cnt = core::ptr::read(ptr.add(1));
        let insns_ptr = core::ptr::read(ptr.add(2)) as u64;
        let license_ptr = core::ptr::read(ptr.add(4)) as u64;
        let log_level = core::ptr::read(ptr.add(6));
        let log_size = core::ptr::read(ptr.add(7));
        let log_buf = core::ptr::read(ptr.add(8)) as u64;
        let kern_version = core::ptr::read(ptr.add(10));
        let prog_flags = core::ptr::read(ptr.add(11));
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
    let insn_bytes = insn_cnt as usize * 8;
    let raw_insns = unsafe { core::slice::from_raw_parts(insns_ptr as *const u8, insn_bytes) };
    let mut insns = Vec::new();
    for chunk in raw_insns.chunks_exact(8) {
        let arr: [u8; 8] = chunk.try_into().unwrap();
        insns.push(bpf_insn::BpfInsn::from_bytes(&arr));
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

pub fn sys_bpf(cmd: u64, uattr: usize, size: u32) -> AxResult<isize> {
    match cmd {
        cmd::MAP_CREATE => handle_map_create(uattr, size),
        cmd::PROG_LOAD => handle_prog_load(uattr, size),
        cmd::MAP_LOOKUP_ELEM => handle_map_lookup_elem(uattr, size),
        cmd::MAP_UPDATE_ELEM => handle_map_update_elem(uattr, size),
        cmd::MAP_DELETE_ELEM => handle_map_delete_elem(uattr, size),
        cmd::MAP_GET_NEXT_KEY => handle_map_get_next_key(uattr, size),
        cmd::RAW_TRACEPOINT_OPEN => handle_raw_tracepoint_open(uattr, size),
        cmd::OBJ_PIN | cmd::OBJ_GET => {
            warn!("bpf: obj pin/get not yet implemented");
            Err(bpf_error::EINVAL)
        }
        cmd::PROG_ATTACH | cmd::PROG_DETACH | cmd::LINK_CREATE => {
            warn!("bpf: prog attach/detach/link not yet implemented");
            Err(bpf_error::EINVAL)
        }
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
    _attr_uptr: usize,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> AxResult<isize> {
    warn!(
        "perf_event_open: pid={pid}, cpu={cpu}, group_fd={group_fd}, flags={flags:#x} not yet \
         implemented"
    );
    Err(bpf_error::EINVAL)
}
