use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};

use axerrno::{AxError, AxResult};
use axhal::{
    paging::{MappingFlags, PageSize},
    time::monotonic_time_nanos,
};
use axmm::backend::{Backend, SharedPages};
use axsync::Mutex;
use axtask::current;
use linux_raw_sys::{ctypes::c_ushort, general::*};
use memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use starry_process::Pid;

use super::{IPC_PRIVATE, IPC_RMID, IPC_SET, IPC_STAT, IpcPerm, next_ipc_id};
use crate::{
    mm::{UserPtr, nullable},
    task::AsThread,
};

bitflags::bitflags! {
    /// flags for sys_shmat
    #[derive(Debug)]
    struct ShmAtFlags: u32 {
        /* attach read-only else read-write */
        const SHM_RDONLY = 0o10000;
        /* round attach address to SHMLBA */
        const SHM_RND = 0o20000;
        /* take-over region on attach */
        const SHM_REMAP = 0o40000;
    }
}

/// Data structure describing a shared memory segment.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ShmidDs {
    /// operation permission struct
    shm_perm: IpcPerm,
    /// size of segment in bytes
    shm_segsz: __kernel_size_t,
    /// time of last shmat()
    shm_atime: __kernel_time_t,
    /// time of last shmdt()
    shm_dtime: __kernel_time_t,
    /// time of last change by shmctl()
    pub shm_ctime: __kernel_time_t,
    /// pid of creator
    shm_cpid: __kernel_pid_t,
    /// pid of last shmop
    shm_lpid: __kernel_pid_t,
    /// number of current attaches
    shm_nattch: c_ushort,
}

impl ShmidDs {
    fn new(key: i32, size: usize, mode: __kernel_mode_t, pid: __kernel_pid_t) -> Self {
        Self {
            shm_perm: IpcPerm {
                key,
                uid: 0,
                gid: 0,
                cuid: 0,
                cgid: 0,
                mode,
                seq: 0,
                pad: 0,
                unused0: 0,
                unused1: 0,
            },
            shm_segsz: size as __kernel_size_t,
            shm_atime: 0,
            shm_dtime: 0,
            shm_ctime: 0,
            shm_cpid: pid,
            shm_lpid: pid,
            shm_nattch: 0,
        }
    }
}

/// This struct is used to maintain the shmem in kernel.
pub struct ShmInner {
    /// Shared memory segment identifier.
    pub shmid: i32,
    /// Number of pages in the shared memory segment.
    pub page_num: usize,
    va_range: BTreeMap<Pid, VirtAddrRange>,
    /// physical pages
    pub phys_pages: Option<Arc<SharedPages>>,
    /// whether remove on last detach, see shm_ctl
    pub rmid: bool,
    /// Mapping flags used for this shared memory segment.
    pub mapping_flags: MappingFlags,
    /// c type struct, used in shm_ctl
    pub shmid_ds: ShmidDs,
}

impl ShmInner {
    /// Creates a new [`ShmInner`].
    pub fn new(key: i32, shmid: i32, size: usize, mapping_flags: MappingFlags, pid: Pid) -> Self {
        ShmInner {
            shmid,
            page_num: memory_addr::align_up_4k(size) / PAGE_SIZE_4K,
            va_range: BTreeMap::new(),
            phys_pages: None,
            rmid: false,
            mapping_flags,
            shmid_ds: ShmidDs::new(
                key,
                size,
                mapping_flags.bits() as __kernel_mode_t,
                pid as __kernel_pid_t,
            ),
        }
    }

    /// Updates the pid of last shmop and checks if the size and mapping flags
    /// match.
    pub fn try_update(
        &mut self,
        size: usize,
        mapping_flags: MappingFlags,
        pid: Pid,
    ) -> AxResult<isize> {
        if size as __kernel_size_t != self.shmid_ds.shm_segsz
            || mapping_flags.bits() as __kernel_mode_t != self.shmid_ds.shm_perm.mode
        {
            return Err(AxError::InvalidInput);
        }
        self.shmid_ds.shm_lpid = pid as i32;
        Ok(self.shmid as isize)
    }

    /// Maps the given physical shared pages to this shared memory segment.
    pub fn map_to_phys(&mut self, phys_pages: Arc<SharedPages>) {
        self.phys_pages = Some(phys_pages);
    }

    /// Returns the number of processes currently attached to this shared memory
    /// segment.
    pub fn attach_count(&self) -> usize {
        self.va_range.len()
    }

    /// Returns the virtual address range associated with the given Pid.
    pub fn get_addr_range(&self, pid: Pid) -> Option<VirtAddrRange> {
        self.va_range.get(&pid).cloned()
    }

    /// Called by sys_shmat
    pub fn attach_process(&mut self, pid: Pid, va_range: VirtAddrRange) {
        assert!(self.get_addr_range(pid).is_none());
        self.va_range.insert(pid, va_range);
        self.shmid_ds.shm_nattch += 1;
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_atime = monotonic_time_nanos() as __kernel_time_t;
    }

    /// Called by sys_shmdt
    pub fn detach_process(&mut self, pid: Pid) {
        assert!(self.get_addr_range(pid).is_some());
        self.va_range.remove(&pid);
        self.shmid_ds.shm_nattch -= 1;
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_dtime = monotonic_time_nanos() as __kernel_time_t;
    }
}

/// A bidirectional BTreeMap, allowing lookup by key or value.
/// TODO: I don't know where to put this, so I put it here.
#[derive(Debug, Clone)]
pub struct BiBTreeMap<K, V>
where
    K: Ord + Clone,
    V: Ord + Clone,
{
    forward: BTreeMap<K, V>,
    reverse: BTreeMap<V, K>,
}

impl<K, V> BiBTreeMap<K, V>
where
    K: Ord + Clone,
    V: Ord + Clone,
{
    /// Creates a new empty [`BiBTreeMap`].
    pub const fn new() -> Self {
        BiBTreeMap {
            forward: BTreeMap::new(),
            reverse: BTreeMap::new(),
        }
    }

    /// Inserts a key-value pair into the map, replacing any existing mapping
    /// for either key or value.
    pub fn insert(&mut self, key: K, value: V) {
        if let Some(old_key) = self.reverse.insert(value.clone(), key.clone()) {
            self.forward.remove(&old_key);
        }
        if let Some(old_value) = self.forward.insert(key, value.clone()) {
            self.reverse.remove(&old_value);
        }
    }

    /// Returns a reference to the value corresponding to the given key, if it
    /// exists.
    pub fn get_by_key(&self, key: &K) -> Option<&V> {
        self.forward.get(key)
    }

    /// Returns a reference to the key corresponding to the given value, if it
    /// exists.
    pub fn get_by_value(&self, value: &V) -> Option<&K> {
        self.reverse.get(value)
    }

    /// Removes a key-value pair by key, returning the value if it existed.
    pub fn remove_by_key(&mut self, key: &K) -> Option<V> {
        if let Some(value) = self.forward.remove(key) {
            self.reverse.remove(&value);
            Some(value)
        } else {
            None
        }
    }

    /// Removes a key-value pair by value, returning the key if it existed.
    pub fn remove_by_value(&mut self, value: &V) -> Option<K> {
        if let Some(key) = self.reverse.remove(value) {
            self.forward.remove(&key);
            Some(key)
        } else {
            None
        }
    }
}

impl<K, V> Default for BiBTreeMap<K, V>
where
    K: Ord + Clone,
    V: Ord + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

/// This struct is used to manage the relationship between the shmem and
/// processes. note: this struct do not modify the struct ShmInner, but only
/// manage the mapping.
pub struct ShmManager {
    /// key <-> shm_id
    key_shmid: BiBTreeMap<i32, i32>,
    /// shm_id -> shm_inner
    shmid_inner: BTreeMap<i32, Arc<Mutex<ShmInner>>>,
    /// pid -> shm_id <-> vaddr
    pid_shmid_vaddr: BTreeMap<Pid, BiBTreeMap<i32, VirtAddr>>,
}

impl ShmManager {
    const fn new() -> Self {
        ShmManager {
            key_shmid: BiBTreeMap::new(),
            shmid_inner: BTreeMap::new(),
            pid_shmid_vaddr: BTreeMap::new(),
        }
    }

    /// Returns the shared memory ID associated with the given key.
    pub fn get_shmid_by_key(&self, key: i32) -> Option<i32> {
        self.key_shmid.get_by_key(&key).cloned()
    }

    /// Returns the shared memory inner structure [`ShmInner`] associated with
    /// the given shared memory ID.
    pub fn get_inner_by_shmid(&self, shmid: i32) -> Option<Arc<Mutex<ShmInner>>> {
        self.shmid_inner.get(&shmid).cloned()
    }

    /// Returns the shared memory ID associated with the given pid and virtual
    /// address.
    pub fn get_shmid_by_vaddr(&self, pid: Pid, vaddr: VirtAddr) -> Option<i32> {
        self.pid_shmid_vaddr
            .get(&pid)
            .and_then(|map| map.get_by_value(&vaddr))
            .cloned()
    }

    fn get_shmids_by_pid(&self, pid: Pid) -> Option<Vec<i32>> {
        let map = self.pid_shmid_vaddr.get(&pid)?;
        let mut res = Vec::new();
        for key in map.forward.keys() {
            res.push(*key);
        }
        Some(res)
    }

    // used by garbage collection
    #[allow(dead_code)]
    fn find_vaddr_by_shmid(&self, pid: Pid, shmid: i32) -> Option<VirtAddr> {
        self.pid_shmid_vaddr
            .get(&pid)
            .and_then(|map| map.get_by_key(&shmid))
            .cloned()
    }

    /// Inserts a mapping from a key to a shared memory ID.
    pub fn insert_key_shmid(&mut self, key: i32, shmid: i32) {
        self.key_shmid.insert(key, shmid);
    }

    /// Inserts a mapping from a shared memory ID to its inner
    /// structure [`ShmInner`].
    pub fn insert_shmid_inner(&mut self, shmid: i32, shm_inner: Arc<Mutex<ShmInner>>) {
        self.shmid_inner.insert(shmid, shm_inner);
    }

    /// Inserts a mapping from a process and shared memory ID to a virtual
    /// address.
    pub fn insert_shmid_vaddr(&mut self, pid: Pid, shmid: i32, vaddr: VirtAddr) {
        // maintain the map 'shmid_vaddr'
        self.pid_shmid_vaddr
            .entry(pid)
            .or_default()
            .insert(shmid, vaddr);
    }

    /// Removes the mapping from a process and shared memory address.
    pub fn remove_shmaddr(&mut self, pid: Pid, shmaddr: VirtAddr) {
        let mut empty: bool = false;
        if let Some(map) = self.pid_shmid_vaddr.get_mut(&pid) {
            map.remove_by_value(&shmaddr);
            empty = map.forward.is_empty();
        }
        if empty {
            self.pid_shmid_vaddr.remove(&pid);
        }
    }

    // called when a process exit
    fn remove_pid(&mut self, pid: Pid) {
        self.pid_shmid_vaddr.remove(&pid);
    }

    /// Removes the shared memory segment.
    pub fn remove_shmid(&mut self, shmid: i32) {
        self.key_shmid.remove_by_value(&shmid);
        self.shmid_inner.remove(&shmid);
        // for map in self.pid_shmid_vaddr.values() {
        // assert!(map.get_by_key(&shmid).is_none());
        // }
    }

    /// Clear all shared memory segments related to the process.
    pub fn clear_proc_shm(&mut self, pid: Pid) {
        if let Some(shmids) = self.get_shmids_by_pid(pid) {
            for shmid in shmids {
                if let Some(shm_inner) = self.get_inner_by_shmid(shmid) {
                    let mut shm_inner = shm_inner.lock();
                    shm_inner.detach_process(pid);
                    if shm_inner.rmid && shm_inner.attach_count() == 0 {
                        self.remove_shmid(shmid);
                    }
                }
            }
        }
        self.remove_pid(pid);
    }
}

/// Global shared memory manager.
pub static SHM_MANAGER: Mutex<ShmManager> = Mutex::new(ShmManager::new());

pub fn sys_shmget(key: i32, size: usize, shmflg: usize) -> AxResult<isize> {
    let page_num = memory_addr::align_up_4k(size) / PAGE_SIZE_4K;
    if page_num == 0 {
        return Err(AxError::InvalidInput);
    }

    let mut mapping_flags = MappingFlags::from_name("USER").unwrap();
    if shmflg & 0o400 != 0 {
        mapping_flags.insert(MappingFlags::READ);
    }
    if shmflg & 0o200 != 0 {
        mapping_flags.insert(MappingFlags::WRITE);
    }
    if shmflg & 0o100 != 0 {
        mapping_flags.insert(MappingFlags::EXECUTE);
    }

    let cur_pid = current().as_thread().proc_data.proc.pid();
    let mut shm_manager = SHM_MANAGER.lock();

    if key != IPC_PRIVATE {
        // This process has already created a shared memory segment with the same key
        if let Some(shmid) = shm_manager.get_shmid_by_key(key) {
            let shm_inner = shm_manager
                .get_inner_by_shmid(shmid)
                .ok_or(AxError::InvalidInput)?;
            let mut shm_inner = shm_inner.lock();
            return shm_inner.try_update(size, mapping_flags, cur_pid);
        }
    }

    // Create a new shm_inner
    let shmid = next_ipc_id();
    let shm_inner = Arc::new(Mutex::new(ShmInner::new(
        key,
        shmid,
        size,
        mapping_flags,
        cur_pid,
    )));
    shm_manager.insert_key_shmid(key, shmid);
    shm_manager.insert_shmid_inner(shmid, shm_inner);

    Ok(shmid as isize)
}

pub fn sys_shmat(shmid: i32, addr: usize, shmflg: u32) -> AxResult<isize> {
    let shm_inner = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager.get_inner_by_shmid(shmid).unwrap()
    };
    let mut shm_inner = shm_inner.lock();
    let mut mapping_flags = shm_inner.mapping_flags;
    let shm_flg = ShmAtFlags::from_bits_truncate(shmflg);

    if shm_flg.contains(ShmAtFlags::SHM_RDONLY) {
        mapping_flags.remove(MappingFlags::WRITE);
    }

    // TODO: solve shmflg: SHM_RND and SHM_REMAP

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let pid = proc_data.proc.pid();
    let mut aspace = proc_data.aspace.lock();

    let start_aligned = memory_addr::align_down_4k(addr);
    let length = shm_inner.page_num * PAGE_SIZE_4K;

    // alloc the virtual address range
    assert!(shm_inner.get_addr_range(pid).is_none());
    let start_addr = aspace
        .find_free_area(
            VirtAddr::from(start_aligned),
            length,
            VirtAddrRange::new(aspace.base(), aspace.end()),
            PAGE_SIZE_4K,
        )
        .or_else(|| {
            aspace.find_free_area(
                aspace.base(),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
                PAGE_SIZE_4K,
            )
        })
        .ok_or(AxError::NoMemory)?;
    let end_addr = VirtAddr::from(start_addr.as_usize() + length);
    let va_range = VirtAddrRange::new(start_addr, end_addr);

    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.insert_shmid_vaddr(pid, shm_inner.shmid, start_addr);
    info!(
        "Process {} alloc shm virt addr start: {:#x}, size: {}, mapping_flags: {:#x?}",
        pid,
        start_addr.as_usize(),
        length,
        mapping_flags
    );

    // map the virtual address range to the physical address
    if let Some(phys_pages) = shm_inner.phys_pages.clone() {
        // Another proccess has attached the shared memory
        // TODO(mivik): shm page size
        let backend = Backend::new_shared(start_addr, phys_pages);
        aspace.map(start_addr, length, mapping_flags, false, backend)?;
    } else {
        // This is the first process to attach the shared memory
        let pages = Arc::new(SharedPages::new(length, PageSize::Size4K)?);
        let backend = Backend::new_shared(start_addr, pages.clone());
        aspace.map(start_addr, length, mapping_flags, false, backend)?;

        shm_inner.map_to_phys(pages);
    }

    shm_inner.attach_process(pid, va_range);
    Ok(start_addr.as_usize() as isize)
}

pub fn sys_shmctl(shmid: i32, cmd: u32, buf: UserPtr<ShmidDs>) -> AxResult<isize> {
    let shm_inner = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_inner_by_shmid(shmid)
            .ok_or(AxError::InvalidInput)?
    };
    let mut shm_inner = shm_inner.lock();

    let cmd = cmd as i32;
    if cmd == IPC_SET {
        shm_inner.shmid_ds = *buf.get_as_mut()?;
    } else if cmd == IPC_STAT {
        if let Some(shmid_ds) = nullable!(buf.get_as_mut())? {
            *shmid_ds = shm_inner.shmid_ds;
        }
    } else if cmd == IPC_RMID {
        shm_inner.rmid = true;
    } else {
        return Err(AxError::InvalidInput);
    }

    shm_inner.shmid_ds.shm_ctime = monotonic_time_nanos() as __kernel_time_t;
    Ok(0)
}

// Garbage collection for shared memory:
// 1. when the process call sys_shmdt, delete everything related to shmaddr,
//    including map 'shmid_vaddr';
// 2. when the last process detach the shared memory and this shared memory was
//    specified with IPC_RMID, delete everything related to this shared memory,
//    including all the 3 maps;
// 3. when a process exit, delete everything related to this process, including
//    2 maps: 'shmid_vaddr' and 'shmid_inner';
//
// The attach between the process and the shared memory occurs in sys_shmat,
//  and the detach occurs in sys_shmdt, or when the process exits.

// Note: all the below delete functions only delete the mapping between the
// shm_id and the shm_inner,   but the shm_inner is not deleted or modifyed!
pub fn sys_shmdt(shmaddr: usize) -> AxResult<isize> {
    let shmaddr = VirtAddr::from(shmaddr);

    let curr = current();
    let proc_data = &curr.as_thread().proc_data;

    let pid = proc_data.proc.pid();
    let shmid = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_shmid_by_vaddr(pid, shmaddr)
            .ok_or(AxError::InvalidInput)?
    };

    let shm_inner = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_inner_by_shmid(shmid)
            .ok_or(AxError::InvalidInput)?
    };
    let mut shm_inner = shm_inner.lock();
    let va_range = shm_inner.get_addr_range(pid).ok_or(AxError::InvalidInput)?;

    let mut aspace = proc_data.aspace.lock();
    aspace.unmap(va_range.start, va_range.size())?;

    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.remove_shmaddr(pid, shmaddr);
    shm_inner.detach_process(pid);

    if shm_inner.rmid && shm_inner.attach_count() == 0 {
        shm_manager.remove_shmid(shmid);
    }

    Ok(0)
}
