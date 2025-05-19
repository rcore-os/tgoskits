use alloc::collections::btree_map::BTreeMap;
use alloc::vec::Vec;

use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axhal::time::monotonic_time_nanos;
use axmm::SharedPages;
use axprocess::Pid;
use axsync::Mutex;
use axtask::{TaskExtRef, current};

use lazy_static::lazy_static;
use linux_raw_sys::ctypes::{c_long, c_ushort};
use linux_raw_sys::general::*;
use memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use page_table_entry::MappingFlags;

use crate::imp::ipc::{BiBTreeMap, IPCID_ALLOCATOR};
use crate::ptr::{UserPtr, nullable};

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

/// flags for sys_shmget, sys_msgget, sys_semget
pub const IPC_PRIVATE: i32 = 0;
pub const IPC_RMID: u32 = 0;
pub const IPC_SET: u32 = 1;
pub const IPC_STAT: u32 = 2;

/// Data structure used to pass permission information to IPC operations.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    pub key: __kernel_key_t,
    pub uid: __kernel_uid_t,
    pub gid: __kernel_gid_t,
    pub cuid: __kernel_uid_t,
    pub cgid: __kernel_gid_t,
    pub mode: __kernel_mode_t,
    pub seq: c_ushort,
    pad: c_ushort,   // for memory align
    unused0: c_long, // for memory align
    unused1: c_long, // for memory align
}

/// Data structure describing a shared memory segment.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ShmidDs {
    pub shm_perm: IpcPerm,          /* operation permission struct */
    pub shm_segsz: __kernel_size_t, /* size of segment in bytes */
    pub shm_atime: __kernel_time_t, /* time of last shmat() */
    pub shm_dtime: __kernel_time_t, /* time of last shmdt() */
    pub shm_ctime: __kernel_time_t, /* time of last change by shmctl() */
    pub shm_cpid: __kernel_pid_t,   /* pid of creator */
    pub shm_lpid: __kernel_pid_t,   /* pid of last shmop */
    pub shm_nattch: c_ushort,       /* number of current attaches */
}

impl ShmidDs {
    pub fn new(key: i32, size: usize, mode: __kernel_mode_t, pid: __kernel_pid_t) -> Self {
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

/**
 * This struct is used to maintain the shmem in kernel.
 */
struct ShmInner {
    pub shmid: i32,
    pub page_num: usize,
    pub va_range: BTreeMap<Pid, VirtAddrRange>, // In each process, this shm is mapped into different virt addr range
    pub phys_pages: Option<Arc<SharedPages>>,   // shm page num -> physical page
    pub rmid: bool,                             // whether remove on last detach, see shm_ctl
    pub mapping_flags: MappingFlags,
    pub shmid_ds: ShmidDs, // c type struct, used in shm_ctl
}

impl ShmInner {
    fn new(key: i32, shmid: i32, size: usize, mapping_flags: MappingFlags, pid: Pid) -> Self {
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

    pub fn try_update(
        &mut self,
        size: usize,
        mapping_flags: MappingFlags,
        pid: Pid,
    ) -> LinuxResult<isize> {
        if size as __kernel_size_t != self.shmid_ds.shm_segsz
            || mapping_flags.bits() as __kernel_mode_t != self.shmid_ds.shm_perm.mode
        {
            return Err(LinuxError::EINVAL);
        }
        self.shmid_ds.shm_lpid = pid as i32;
        Ok(self.shmid as isize)
    }

    pub fn map_to_phys(&mut self, phys_pages: Arc<SharedPages>) {
        self.phys_pages = Some(phys_pages);
    }

    pub fn attach_count(&self) -> usize {
        self.va_range.len()
    }

    fn get_addr_range(&self, pid: Pid) -> Option<VirtAddrRange> {
        self.va_range.get(&pid).cloned()
    }

    // called by sys_shmat
    fn attach_process(&mut self, pid: Pid, va_range: VirtAddrRange) {
        assert!(self.get_addr_range(pid).is_none());
        self.va_range.insert(pid, va_range);
        self.shmid_ds.shm_nattch += 1;
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_atime = monotonic_time_nanos() as __kernel_time_t;
    }

    // called by sys_shmdt
    fn detach_process(&mut self, pid: Pid) {
        assert!(self.get_addr_range(pid).is_some());
        self.va_range.remove(&pid);
        self.shmid_ds.shm_nattch -= 1;
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_dtime = monotonic_time_nanos() as __kernel_time_t;
    }
}

/**
 * This struct is used to manage the relationship between the shmem and processes.
 * note: this struct do not modify the struct ShmInner, but only manage the mapping.
 */
struct ShmManager {
    key_shmid: BiBTreeMap<i32, i32>,                  // key <-> shm_id
    shmid_inner: BTreeMap<i32, Arc<Mutex<ShmInner>>>, // shm_id -> shm_inner
    pid_shmid_vaddr: BTreeMap<Pid, BiBTreeMap<i32, VirtAddr>>, // in specific process, shm_id <-> shm_start_addr
}

impl ShmManager {
    const fn new() -> Self {
        ShmManager {
            key_shmid: BiBTreeMap::new(),
            shmid_inner: BTreeMap::new(),
            pid_shmid_vaddr: BTreeMap::new(),
        }
    }

    // used by sys_shmget
    fn get_shmid_by_key(&self, key: i32) -> Option<i32> {
        self.key_shmid.get_by_key(&key).cloned()
    }

    // the only way to find shm_inner -- the data structure to maintain shm
    fn get_inner_by_shmid(&self, shmid: i32) -> Option<Arc<Mutex<ShmInner>>> {
        self.shmid_inner.get(&shmid).cloned()
    }

    // used by sys_shmdt
    fn get_shmid_by_vaddr(&self, pid: Pid, vaddr: VirtAddr) -> Option<i32> {
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

    // used by sys_shmget
    pub fn insert_key_shmid(&mut self, key: i32, shmid: i32) {
        self.key_shmid.insert(key, shmid);
    }

    // used by sys_shmat
    pub fn insert_shmid_inner(&mut self, shmid: i32, shm_inner: Arc<Mutex<ShmInner>>) {
        self.shmid_inner.insert(shmid, shm_inner);
    }

    // used by sys_shmat, aiming at garbage collection when called sys_shmdt
    pub fn insert_shmid_vaddr(&mut self, pid: Pid, shmid: i32, vaddr: VirtAddr) {
        // maintain the map 'shmid_vaddr'
        self.pid_shmid_vaddr
            .entry(pid)
            .or_insert_with(BiBTreeMap::new)
            .insert(shmid, vaddr);
    }

    /*
     * Garbage collection for shared memory:
     * 1. when the process call sys_shmdt, delete everything related to shmaddr,
     *   including map 'shmid_vaddr';
     * 2. when the last process detach the shared memory and this shared memory
     *   was specified with IPC_RMID, delete everything related to this shared memory,
     *   including all the 3 maps;
     * 3. when a process exit, delete everything related to this process, including 2
     *   maps: 'shmid_vaddr' and 'shmid_inner';
     *
     *
     * The attach between the process and the shared memory occurs in sys_shmat,
     *  and the detach occurs in sys_shmdt, or when the process exits.
     */

    /*
     * Note: all the below delete functions only delete the mapping between the shm_id and the shm_inner,
     *   but the shm_inner is not deleted or modifyed!
     */

    // called by shmdt
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
    pub fn remove_pid(&mut self, pid: Pid) {
        self.pid_shmid_vaddr.remove(&pid);
    }

    pub fn remove_shmid(&mut self, shmid: i32) {
        self.key_shmid.remove_by_value(&shmid);
        self.shmid_inner.remove(&shmid);
        // for map in self.pid_shmid_vaddr.values() {
        // assert!(map.get_by_key(&shmid).is_none());
        // }
    }
}

lazy_static! {
    static ref SHM_MANAGER: Mutex<ShmManager> = Mutex::new(ShmManager::new());
}

// called when a process exit, detach all the shmem related
pub fn clear_proc_shm(pid: Pid) {
    let mut shm_manager = SHM_MANAGER.lock();
    if let Some(shmids) = shm_manager.get_shmids_by_pid(pid) {
        for shmid in shmids {
            let shm_inner = shm_manager.get_inner_by_shmid(shmid).unwrap();
            let mut shm_inner = shm_inner.lock();
            shm_inner.detach_process(pid);

            if shm_inner.rmid && shm_inner.attach_count() == 0 {
                shm_manager.remove_shmid(shmid);
            }
        }
    }
    shm_manager.remove_pid(pid);
}

pub fn sys_shmget(key: i32, size: usize, shmflg: usize) -> LinuxResult<isize> {
    let page_num = memory_addr::align_up_4k(size) / PAGE_SIZE_4K;
    if page_num == 0 {
        return Err(LinuxError::EINVAL);
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

    let cur_pid = current().task_ext().thread.process().pid();
    let mut shm_manager = SHM_MANAGER.lock();

    if key != IPC_PRIVATE {
        // This process has already created a shared memory segment with the same key
        if let Some(shmid) = shm_manager.get_shmid_by_key(key) {
            let shm_inner = shm_manager
                .get_inner_by_shmid(shmid)
                .ok_or(LinuxError::EINVAL)?;
            let mut shm_inner = shm_inner.lock();
            return shm_inner.try_update(size, mapping_flags, cur_pid);
        }
    }

    // Create a new shm_inner
    let mut shmid_allocator = IPCID_ALLOCATOR.lock();
    let shmid = shmid_allocator.alloc();
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

pub fn sys_shmat(shmid: i32, addr: usize, shmflg: u32) -> LinuxResult<isize> {
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
    let cur_pid = curr.task_ext().thread.process().pid();
    let process_data = curr.task_ext().process_data();
    let mut aspace = process_data.aspace.lock();

    let start_aligned = memory_addr::align_down_4k(addr);
    let length = shm_inner.page_num * PAGE_SIZE_4K;

    // alloc the virtual address range
    assert!(shm_inner.get_addr_range(cur_pid).is_none());
    let start_addr = aspace
        .find_free_area(
            VirtAddr::from(start_aligned),
            length,
            VirtAddrRange::new(aspace.base(), aspace.end()),
        )
        .or_else(|| {
            aspace.find_free_area(
                aspace.base(),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            )
        })
        .ok_or(LinuxError::ENOMEM)?;
    let end_addr = VirtAddr::from(start_addr.as_usize() + length);
    let va_range = VirtAddrRange::new(start_addr, end_addr);

    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.insert_shmid_vaddr(cur_pid, shm_inner.shmid, start_addr);
    info!(
        "Process {} alloc shm virt addr start: {:#x}, size: {}, mapping_flags: {:#x?}",
        cur_pid,
        start_addr.as_usize(),
        length,
        mapping_flags
    );

    // map the virtual address range to the physical address
    if let Some(phys_pages) = shm_inner.phys_pages.clone() {
        // Another proccess has attached the shared memory
        aspace.map_shared(start_addr, length, mapping_flags, Some(phys_pages))?;
    } else {
        // This is the first process to attach the shared memory
        let result = aspace.map_shared(start_addr, length, mapping_flags, None);

        match result {
            Ok(pages) => {
                info!(
                    "proc {} map shm addr: {:#x}, size: {}",
                    cur_pid,
                    start_addr.as_usize(),
                    length
                );
                shm_inner.map_to_phys(pages);
            }
            Err(e) => {
                error!(
                    "proc {} map shm addr: {:#x}, size: {}, error: {:?}",
                    cur_pid,
                    start_addr.as_usize(),
                    length,
                    e
                );
                return Err(LinuxError::ENOMEM);
            }
        }
    }

    shm_inner.attach_process(cur_pid, va_range);
    Ok(start_addr.as_usize() as isize)
}

pub fn sys_shmctl(shmid: i32, cmd: u32, buf: UserPtr<ShmidDs>) -> LinuxResult<isize> {
    let shm_inner = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_inner_by_shmid(shmid)
            .ok_or(LinuxError::EINVAL)?
    };
    let mut shm_inner = shm_inner.lock();

    if cmd == IPC_SET {
        shm_inner.shmid_ds = *buf.get_as_mut()?;
    } else if cmd == IPC_STAT {
        if let Some(shmid_ds) = nullable!(buf.get_as_mut())? {
            *shmid_ds = shm_inner.shmid_ds;
        }
    } else if cmd == IPC_RMID {
        shm_inner.rmid = true;
    } else {
        return Err(LinuxError::EINVAL);
    }

    shm_inner.shmid_ds.shm_ctime = monotonic_time_nanos() as __kernel_time_t;
    Ok(0)
}

pub fn sys_shmdt(shmaddr: usize) -> LinuxResult<isize> {
    let shmaddr = VirtAddr::from(shmaddr);
    let pid = {
        let curr = current();
        curr.task_ext().thread.process().pid()
    };
    let shmid = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_shmid_by_vaddr(pid, shmaddr)
            .ok_or(LinuxError::EINVAL)?
    };

    let shm_inner = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_inner_by_shmid(shmid)
            .ok_or(LinuxError::EINVAL)?
    };
    let mut shm_inner = shm_inner.lock();
    let va_range = shm_inner.get_addr_range(pid).ok_or(LinuxError::EINVAL)?;

    let curr = current();
    let mut aspace = curr.task_ext().process_data().aspace.lock();
    aspace.unmap(va_range.start, va_range.size())?;
    axhal::arch::flush_tlb(None);

    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.remove_shmaddr(pid, shmaddr);
    shm_inner.detach_process(pid);

    if shm_inner.rmid && shm_inner.attach_count() == 0 {
        shm_manager.remove_shmid(shmid);
    }

    Ok(0)
}
