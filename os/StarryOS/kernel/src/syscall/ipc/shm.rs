use alloc::{
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
    sync::Arc,
    vec::Vec,
};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{
    paging::{MappingFlags, PageSize},
    time::monotonic_time_nanos,
};
use ax_sync::PiMutex;
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::*;
use starry_process::Pid;
use starry_vm::VmMutPtr;

use super::{
    IPC_CREAT, IPC_EXCL, IPC_INFO, IPC_PRIVATE, IPC_RMID, IPC_SET, IPC_STAT, IpcPerm, SHM_INFO,
    SHM_STAT, has_ipc_permission, next_ipc_id,
};
use crate::{
    mm::{AddrSpace, Backend, SharedPages, UserPtr},
    task::current_user_task,
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
#[derive(Clone, Copy, AnyBitPattern, bytemuck::NoUninit)]
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
    ///
    /// Linux `shmid64_ds` declares this as `__kernel_ulong_t` (8 bytes on every
    /// 64-bit arch), NOT `unsigned short`. A narrow field here left the high
    /// bytes of glibc's `shm_nattch` read uninitialized (garbage attach count).
    shm_nattch: __kernel_ulong_t,
    /// Trailing reserved field present in Linux `shmid64_ds` (`__unused4`).
    __unused4: __kernel_ulong_t,
    /// Trailing reserved field present in Linux `shmid64_ds` (`__unused5`).
    __unused5: __kernel_ulong_t,
}

// `struct shmid64_ds` (asm-generic, shared by aarch64/riscv64/loongarch64 and
// layout-identical on x86-64): `ipc64_perm` + segsz + 3×time + 2×pid + nattch +
// 2× trailing reserved word. Guard against accidental re-narrowing or padding.
const _: () = assert!(
    core::mem::size_of::<ShmidDs>() == core::mem::size_of::<IpcPerm>() + 64,
    "ShmidDs must match Linux shmid64_ds layout"
);

impl ShmidDs {
    fn new(
        key: i32,
        size: usize,
        mode: __kernel_mode_t,
        pid: __kernel_pid_t,
        uid: u32,
        gid: u32,
    ) -> Self {
        Self {
            shm_perm: IpcPerm {
                key,
                uid,
                gid,
                cuid: uid,
                cgid: gid,
                mode,
                seq: 0,
                pad: 0,
                alignment_pad: 0,
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
            __unused4: 0,
            __unused5: 0,
        }
    }
}

/// System-wide shared memory info returned by IPC_INFO.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, bytemuck::NoUninit)]
struct ShmInfo64 {
    shmmax: u64,
    shmmin: u64,
    shmmni: u64,
    shmseg: u64,
    shmall: u64,
}

/// Shared memory usage info returned by SHM_INFO.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, bytemuck::NoUninit)]
struct ShmInfo {
    used_ids: i32,
    _pad: i32,
    shm_tot: u64,
    shm_rss: u64,
    shm_swp: u64,
    swap_attempts: u64,
    swap_successes: u64,
}

/// This struct is used to maintain the shmem in kernel.
pub struct ShmInner {
    /// Shared memory segment identifier.
    pub shmid: i32,
    /// Number of pages in the shared memory segment.
    pub page_num: usize,
    va_range: BTreeMap<Pid, Vec<VirtAddrRange>>,
    /// physical pages
    pub phys_pages: Option<Arc<SharedPages>>,
    /// whether remove on last detach, see shm_ctl
    pub rmid: bool,
    /// Mapping flags used for this shared memory segment.
    pub mapping_flags: MappingFlags,
    /// c type struct, used in shm_ctl
    pub shmid_ds: ShmidDs,
    /// IPC namespace ID that owns this segment
    pub ns_id: u64,
}

impl ShmInner {
    /// Creates a new [`ShmInner`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: i32,
        shmid: i32,
        size: usize,
        shmflg: usize,
        pid: Pid,
        uid: u32,
        gid: u32,
        ns_id: u64,
    ) -> Self {
        let ipc_mode = (shmflg & 0o777) as u16;

        let mut mapping_flags = MappingFlags::from_name("USER").unwrap();
        if shmflg & 0o400 != 0 {
            mapping_flags.insert(MappingFlags::READ);
        }
        if shmflg & 0o200 != 0 {
            // RISC-V reserves W=1,R=0 for leaf PTEs; WRITE implies READ here so
            // the riscv64 PTE layer can auto-correct. This is a page-table-level
            // workaround and must NOT leak into the user-visible IPC mode.
            mapping_flags.insert(MappingFlags::WRITE | MappingFlags::READ);
        }
        if shmflg & 0o100 != 0 {
            mapping_flags.insert(MappingFlags::EXECUTE);
        }

        ShmInner {
            shmid,
            page_num: ax_memory_addr::align_up_4k(size) / PAGE_SIZE_4K,
            va_range: BTreeMap::new(),
            phys_pages: None,
            rmid: false,
            mapping_flags,
            shmid_ds: ShmidDs::new(
                key,
                size,
                ipc_mode as __kernel_mode_t,
                pid as __kernel_pid_t,
                uid,
                gid,
            ),
            ns_id,
        }
    }

    /// Validates a `shmget` against an existing segment and records the
    /// pid of the last operation.
    ///
    /// Mirrors Linux `shm_more_checks()`: the call is rejected with
    /// `EINVAL` only when the requested size is larger than the segment.
    /// The permission bits passed in `shmflg` do not have to match those
    /// used when the segment was created.
    pub fn try_update(&mut self, size: usize, pid: Pid) -> AxResult<isize> {
        if size as __kernel_size_t > self.shmid_ds.shm_segsz {
            return Err(AxError::InvalidInput);
        }
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        Ok(self.shmid as isize)
    }

    /// Maps the given physical shared pages to this shared memory segment.
    pub fn map_to_phys(&mut self, phys_pages: Arc<SharedPages>) {
        self.phys_pages = Some(phys_pages);
    }

    /// Returns the number of current attaches to this shared memory segment.
    pub fn attach_count(&self) -> usize {
        self.va_range.values().map(Vec::len).sum()
    }

    /// Returns all virtual address ranges associated with the given Pid.
    pub fn get_addr_ranges(&self, pid: Pid) -> Vec<VirtAddrRange> {
        self.va_range.get(&pid).cloned().unwrap_or_default()
    }

    /// Returns the virtual address range that starts at the given address.
    pub fn get_addr_range_by_start(&self, pid: Pid, vaddr: VirtAddr) -> Option<VirtAddrRange> {
        self.va_range
            .get(&pid)?
            .iter()
            .find(|range| range.start == vaddr)
            .copied()
    }

    /// Attach a process to this segment.
    pub fn attach_process(&mut self, pid: Pid, va_range: VirtAddrRange) {
        self.va_range.entry(pid).or_default().push(va_range);
        self.shmid_ds.shm_nattch = self.shmid_ds.shm_nattch.saturating_add(1);
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_atime = monotonic_time_nanos() as __kernel_time_t;
    }

    /// Detach a single attach range from this segment. Returns `false` if the
    /// range was already detached (e.g. by a concurrent clear_proc_shm).
    pub fn detach_process_range(&mut self, pid: Pid, vaddr: VirtAddr) -> bool {
        let Some(ranges) = self.va_range.get_mut(&pid) else {
            return false;
        };
        let Some(index) = ranges.iter().position(|range| range.start == vaddr) else {
            return false;
        };
        ranges.remove(index);
        let empty = ranges.is_empty();
        if empty {
            self.va_range.remove(&pid);
        }
        self.shmid_ds.shm_nattch = self.shmid_ds.shm_nattch.saturating_sub(1);
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_dtime = monotonic_time_nanos() as __kernel_time_t;
        true
    }

    /// Detach all attach ranges owned by a process from this segment.
    pub fn detach_process(&mut self, pid: Pid) -> usize {
        let Some(ranges) = self.va_range.remove(&pid) else {
            return 0;
        };
        let attach_count = ranges.len();
        self.shmid_ds.shm_nattch = self
            .shmid_ds
            .shm_nattch
            .saturating_sub(attach_count as __kernel_ulong_t);
        self.shmid_ds.shm_lpid = pid as __kernel_pid_t;
        self.shmid_ds.shm_dtime = monotonic_time_nanos() as __kernel_time_t;
        attach_count
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
    /// (key, ns_id) <-> shm_id
    key_shmid: BiBTreeMap<(i32, u64), i32>,
    /// shm_id -> shm_inner
    shmid_inner: BTreeMap<i32, Arc<PiMutex<ShmInner>>>,
    /// pid -> vaddr -> shm_id
    pid_shmid_vaddr: BTreeMap<Pid, BTreeMap<VirtAddr, i32>>,
}

impl ShmManager {
    const fn new() -> Self {
        ShmManager {
            key_shmid: BiBTreeMap::new(),
            shmid_inner: BTreeMap::new(),
            pid_shmid_vaddr: BTreeMap::new(),
        }
    }

    /// Returns the shared memory ID associated with the given key and IPC
    /// namespace.
    pub fn get_shmid_by_key(&self, key: i32, ns_id: u64) -> Option<i32> {
        self.key_shmid.get_by_key(&(key, ns_id)).cloned()
    }

    /// Returns the shared memory inner structure [`ShmInner`] associated with
    /// the given shared memory ID, validating that it belongs to the specified
    /// IPC namespace.
    pub fn get_inner_by_shmid(&self, shmid: i32, ns_id: u64) -> Option<Arc<PiMutex<ShmInner>>> {
        self.shmid_inner
            .get(&shmid)
            .filter(|inner| inner.lock().ns_id == ns_id)
            .cloned()
    }

    /// Lookup a shm_inner by shmid without namespace validation. Only for
    /// internal cleanup paths (process exit) where the caller has already
    /// scoped the lookup by pid.
    fn get_inner_by_shmid_unchecked(&self, shmid: i32) -> Option<Arc<PiMutex<ShmInner>>> {
        self.shmid_inner.get(&shmid).cloned()
    }

    /// Returns the shared memory ID associated with the given pid and virtual
    /// address.
    pub fn get_shmid_by_vaddr(&self, pid: Pid, vaddr: VirtAddr) -> Option<i32> {
        self.pid_shmid_vaddr
            .get(&pid)
            .and_then(|map| map.get(&vaddr))
            .cloned()
    }

    pub(crate) fn get_shmids_by_pid(&self, pid: Pid) -> Option<Vec<i32>> {
        let map = self.pid_shmid_vaddr.get(&pid)?;
        let mut ids = BTreeSet::new();
        for shmid in map.values() {
            ids.insert(*shmid);
        }
        Some(ids.into_iter().collect())
    }

    /// Inserts a mapping from a (key, ns_id) pair to a shared memory ID.
    pub fn insert_key_shmid(&mut self, key: i32, ns_id: u64, shmid: i32) {
        self.key_shmid.insert((key, ns_id), shmid);
    }

    /// Inserts a mapping from a shared memory ID to its inner
    /// structure [`ShmInner`].
    pub fn insert_shmid_inner(&mut self, shmid: i32, shm_inner: Arc<PiMutex<ShmInner>>) {
        self.shmid_inner.insert(shmid, shm_inner);
    }

    /// Inserts a mapping from a process and shared memory ID to a virtual
    /// address.
    pub fn insert_shmid_vaddr(&mut self, pid: Pid, shmid: i32, vaddr: VirtAddr) {
        self.pid_shmid_vaddr
            .entry(pid)
            .or_default()
            .insert(vaddr, shmid);
    }

    /// Removes the mapping from a process and shared memory address.
    pub fn remove_shmaddr(&mut self, pid: Pid, shmaddr: VirtAddr) {
        let mut empty: bool = false;
        if let Some(map) = self.pid_shmid_vaddr.get_mut(&pid) {
            map.remove(&shmaddr);
            empty = map.is_empty();
        }
        if empty {
            self.pid_shmid_vaddr.remove(&pid);
        }
    }

    /// Remove the pid entry from the pid/shmid/vaddr map.
    pub(crate) fn remove_pid(&mut self, pid: Pid) {
        self.pid_shmid_vaddr.remove(&pid);
    }

    /// Make a segment private by removing its key mapping.
    /// After this, `shmget()` can no longer find the segment by key.
    /// This mirrors Linux's `ipc_set_key_private()`.
    pub fn make_private(&mut self, shmid: i32) {
        self.key_shmid.remove_by_value(&shmid);
    }

    /// Removes the shared memory segment entirely.
    pub fn remove_shmid(&mut self, shmid: i32) {
        self.key_shmid.remove_by_value(&shmid);
        self.shmid_inner.remove(&shmid);
    }
}

/// Global shared memory manager.
///
/// Lock ordering: SHM_MANAGER before ShmInner before aspace (per-process).
/// All code paths must acquire locks in this order to prevent deadlock.
pub static SHM_MANAGER: PiMutex<ShmManager> = PiMutex::new(ShmManager::new());

/// Clear all shared memory segments for a process on exit.
///
/// Collects segment info under SHM_MANAGER, drops the lock, unmaps from
/// aspace, then reacquires SHM_MANAGER for bookkeeping. This keeps the
/// lock ordering consistent with sys_shmget (SHM_MANAGER then ShmInner).
pub fn clear_proc_shm(pid: Pid, aspace: &Arc<PiMutex<AddrSpace>>) {
    // Collect segments attached to this process.
    let segments: Vec<(i32, Arc<PiMutex<ShmInner>>)> = {
        let shm_manager = SHM_MANAGER.lock();
        let shmids = match shm_manager.get_shmids_by_pid(pid) {
            Some(ids) => ids,
            None => return,
        };
        shmids
            .into_iter()
            .filter_map(|shmid| {
                let inner = shm_manager.get_inner_by_shmid_unchecked(shmid)?;
                Some((shmid, inner))
            })
            .collect()
    };

    // Snapshot the VA ranges, then unmap them. SHM_MANAGER is not held
    // here so we don't block other shmget/shmat callers during unmap.
    let mut ranges: Vec<VirtAddrRange> = Vec::new();
    for (_, shm_inner_arc) in &segments {
        let shm_inner = shm_inner_arc.lock();
        ranges.extend(shm_inner.get_addr_ranges(pid));
    }
    if !ranges.is_empty() {
        let mut aspace = aspace.lock();
        for va_range in &ranges {
            let _ = aspace.unmap(va_range.start, va_range.size());
        }
    }

    // Now update the bookkeeping under SHM_MANAGER, then shm_inner.
    let mut shm_manager = SHM_MANAGER.lock();
    for (shmid, shm_inner_arc) in segments {
        let mut shm_inner = shm_inner_arc.lock();
        shm_inner.detach_process(pid);
        if shm_inner.rmid && shm_inner.attach_count() == 0 {
            drop(shm_inner);
            shm_manager.remove_shmid(shmid);
        }
    }
    shm_manager.remove_pid(pid);
}

pub fn sys_shmget(key: i32, size: usize, shmflg: usize) -> AxResult<isize> {
    let curr = current_user_task();
    let thread = curr.as_thread();
    let cur_pid = thread.proc_data.proc.pid();
    let cred = thread.cred();
    let ns_id = thread.proc_data.nsproxy.lock().ipc_ns.lock().ns_id;
    let mut shm_manager = SHM_MANAGER.lock();

    if key != IPC_PRIVATE {
        // A segment already exists for this key.
        if let Some(shmid) = shm_manager.get_shmid_by_key(key, ns_id) {
            // IPC_CREAT | IPC_EXCL requires the creation to fail when the
            // segment is already present. See Linux ipcget_public().
            if shmflg & IPC_CREAT as usize != 0 && shmflg & IPC_EXCL as usize != 0 {
                return Err(AxError::AlreadyExists);
            }
            let shm_inner = shm_manager
                .get_inner_by_shmid(shmid, ns_id)
                .ok_or(AxError::NotFound)?;
            let mut shm_inner = shm_inner.lock();
            return shm_inner.try_update(size, cur_pid);
        }

        // No segment exists for this key: create one only when IPC_CREAT
        // is requested, otherwise the lookup fails with ENOENT.
        if shmflg & IPC_CREAT as usize == 0 {
            return Err(AxError::NotFound);
        }
    }

    // Creating a new segment: its page-rounded size must be non-zero.
    let page_num = ax_memory_addr::align_up_4k(size) / PAGE_SIZE_4K;
    if page_num == 0 {
        return Err(AxError::InvalidInput);
    }

    // Create a new shm_inner
    let shmid = next_ipc_id();
    let shm_inner = Arc::new(PiMutex::new(ShmInner::new(
        key, shmid, size, shmflg, cur_pid, cred.euid, cred.egid, ns_id,
    )));
    shm_manager.insert_key_shmid(key, ns_id, shmid);
    shm_manager.insert_shmid_inner(shmid, shm_inner);

    Ok(shmid as isize)
}

pub fn sys_shmat(shmid: i32, addr: usize, shmflg: u32) -> AxResult<isize> {
    let shm_flg = ShmAtFlags::from_bits_truncate(shmflg);

    let curr = current_user_task();
    let proc_data = &curr.as_thread().proc_data;
    let pid = proc_data.proc.pid();

    info!("shmat pid={pid} shmid={shmid} enter");

    // Grab the shm_inner Arc under SHM_MANAGER, then drop it before
    // mapping work to avoid holding the global lock across aspace ops.
    let shm_inner_arc = {
        let shm_manager = SHM_MANAGER.lock();
        let ns_id = proc_data.nsproxy.lock().ipc_ns.lock().ns_id;
        shm_manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(AxError::InvalidInput)?
    };
    info!("shmat pid={pid} shmid={shmid} lock shm_inner");
    let mut shm_inner = shm_inner_arc.lock();
    let aspace_arc = proc_data.aspace();
    info!("shmat pid={pid} shmid={shmid} lock aspace");
    let mut aspace = aspace_arc.lock();

    let mut mapping_flags = shm_inner.mapping_flags;
    if shm_flg.contains(ShmAtFlags::SHM_RDONLY) {
        mapping_flags.remove(MappingFlags::WRITE);
    }

    // TODO: solve shmflg: SHM_RND and SHM_REMAP

    let start_aligned = ax_memory_addr::align_down_4k(addr);
    let length = shm_inner.page_num * PAGE_SIZE_4K;

    // alloc the virtual address range
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

    info!(
        "Process {} alloc shm virt addr start: {:#x}, size: {}, mapping_flags: {:#x?}",
        pid,
        start_addr.as_usize(),
        length,
        mapping_flags
    );

    // map the virtual address range to the physical address
    if let Some(phys_pages) = shm_inner.phys_pages.clone() {
        // Another process has attached the shared memory
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

    info!("shmat pid={pid} shmid={shmid} mapped; attach_process");
    shm_inner.attach_process(pid, va_range);
    drop(aspace);
    drop(shm_inner);

    info!("shmat pid={pid} shmid={shmid} lock shm_manager for vaddr");
    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.insert_shmid_vaddr(pid, shmid, start_addr);
    info!("shmat pid={pid} shmid={shmid} done");
    Ok(start_addr.as_usize() as isize)
}

pub fn sys_shmctl(shmid: i32, cmd: u32, buf: UserPtr<ShmidDs>) -> AxResult<isize> {
    let cmd = cmd as i32;

    let curr = current_user_task();
    let thread = curr.as_thread();
    let cred = thread.cred();
    let ns_id = thread.proc_data.nsproxy.lock().ipc_ns.lock().ns_id;

    // IPC_INFO: system-wide shared memory limits (no segment lookup).
    if cmd == IPC_INFO {
        let shm_manager = SHM_MANAGER.lock();
        let info = ShmInfo64 {
            shmmax: usize::MAX as u64,
            shmmin: 1,
            shmmni: 4096,
            shmseg: 4096,
            shmall: usize::MAX as u64 / PAGE_SIZE_4K as u64,
        };
        let ptr = buf.as_ptr() as *mut ShmInfo64;
        ptr.vm_write(info)?;
        let ns_count = shm_manager
            .shmid_inner
            .values()
            .filter(|inner| inner.lock().ns_id == ns_id)
            .count();
        let max_idx = ns_count.saturating_sub(1) as isize;
        return Ok(max_idx);
    }

    // SHM_INFO: shared memory usage statistics for this namespace.
    if cmd == SHM_INFO {
        let shm_manager = SHM_MANAGER.lock();
        let mut used_ids: i32 = 0;
        let mut shm_tot: u64 = 0;
        for inner in shm_manager.shmid_inner.values() {
            let guard = inner.lock();
            if guard.ns_id == ns_id {
                used_ids += 1;
                shm_tot += guard.page_num as u64;
            }
        }
        let info = ShmInfo {
            used_ids,
            _pad: 0,
            shm_tot,
            shm_rss: shm_tot,
            shm_swp: 0,
            swap_attempts: 0,
            swap_successes: 0,
        };
        let ptr = buf.as_ptr() as *mut ShmInfo;
        ptr.vm_write(info)?;
        let max_idx = used_ids.saturating_sub(1) as isize;
        return Ok(max_idx);
    }

    // SHM_STAT: return the shmid_ds for the shmid at the given index,
    // counting only segments in this namespace.
    if cmd == SHM_STAT {
        let shm_manager = SHM_MANAGER.lock();
        let result = shm_manager
            .shmid_inner
            .iter()
            .filter(|(_, inner)| inner.lock().ns_id == ns_id)
            .nth(shmid as usize)
            .ok_or(AxError::InvalidInput)
            .and_then(|(actual_shmid, inner)| {
                let guard = inner.lock();
                if !has_ipc_permission(&guard.shmid_ds.shm_perm, cred.euid, cred.egid, false) {
                    return Err(AxError::PermissionDenied);
                }
                let ptr = buf.as_ptr();
                ptr.vm_write(guard.shmid_ds)?;
                Ok(*actual_shmid as isize)
            });
        return result;
    }

    if cmd == IPC_RMID {
        // If no processes are attached, destroy the segment immediately.
        // Otherwise mark it for deferred destruction and remove the key
        // mapping so future shmget() calls won't find it. See Linux
        // do_shm_rmid() in ipc/shm.c.
        let mut shm_manager = SHM_MANAGER.lock();
        let shm_inner_arc = shm_manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(AxError::InvalidInput)?;
        let mut shm_inner = shm_inner_arc.lock();

        shm_inner.rmid = true;
        shm_inner.shmid_ds.shm_ctime = monotonic_time_nanos() as __kernel_time_t;

        // Make private so no new shmget() finds it by key.
        shm_manager.make_private(shmid);

        if shm_inner.attach_count() == 0 {
            drop(shm_inner);
            shm_manager.remove_shmid(shmid);
        }

        return Ok(0);
    }

    // Copy IPC_SET input before taking shared-memory metadata locks. A user
    // fault may sleep and must not retain those locks across the copy.
    let requested = (cmd == IPC_SET).then(|| buf.read()).transpose()?;

    // IPC_SET and IPC_STAT only need shm_inner.
    let shm_inner_arc = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(AxError::InvalidInput)?
    };
    let mut shm_inner = shm_inner_arc.lock();

    let output = if cmd == IPC_SET {
        shm_inner.shmid_ds = requested.expect("IPC_SET input was copied before locking");
        None
    } else if cmd == IPC_STAT {
        (!buf.is_null()).then_some(shm_inner.shmid_ds)
    } else {
        return Err(AxError::InvalidInput);
    };

    shm_inner.shmid_ds.shm_ctime = monotonic_time_nanos() as __kernel_time_t;
    drop(shm_inner);
    if let Some(output) = output {
        buf.write(output)?;
    }
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

    let curr = current_user_task();
    let proc_data = &curr.as_thread().proc_data;
    let pid = proc_data.proc.pid();

    info!("shmdt pid={pid} addr={shmaddr:?} enter");

    // Look up shmid and grab the inner Arc while holding SHM_MANAGER.
    let (shmid, shm_inner_arc) = {
        let shm_manager = SHM_MANAGER.lock();
        let ns_id = proc_data.nsproxy.lock().ipc_ns.lock().ns_id;
        let shmid = shm_manager
            .get_shmid_by_vaddr(pid, shmaddr)
            .ok_or(AxError::InvalidInput)?;
        let shm_inner_arc = shm_manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(AxError::InvalidInput)?;
        (shmid, shm_inner_arc)
    };

    // Snapshot the mapped range for this process.
    let va_range = {
        info!("shmdt pid={pid} lock shm_inner for range");
        let shm_inner = shm_inner_arc.lock();
        shm_inner
            .get_addr_range_by_start(pid, shmaddr)
            .ok_or(AxError::InvalidInput)?
    };

    // Unmap while only holding the aspace lock.
    {
        info!("shmdt pid={pid} lock aspace for unmap");
        let aspace_arc = proc_data.aspace();
        let mut aspace = aspace_arc.lock();
        aspace.unmap(va_range.start, va_range.size())?;
    }

    // Reacquire SHM_MANAGER then shm_inner for bookkeeping, matching
    // the global lock ordering.
    info!("shmdt pid={pid} lock shm_manager for bookkeeping");
    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.remove_shmaddr(pid, shmaddr);
    let mut shm_inner = shm_inner_arc.lock();

    // detach_process_range returns false if clear_proc_shm already detached
    // this pid (race during process exit).
    if shm_inner.detach_process_range(pid, shmaddr)
        && shm_inner.rmid
        && shm_inner.attach_count() == 0
    {
        drop(shm_inner);
        shm_manager.remove_shmid(shmid);
    }

    Ok(0)
}
