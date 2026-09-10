//! SysV shared-memory segment ownership and registry.
//!
//! Syscall decoding stays in syscall::ipc::shm. MM and task teardown use this
//! domain directly; attachment ownership is still being migrated to VMAs.

use alloc::{
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
    sync::Arc,
    vec::Vec,
};

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{paging::MappingFlags, time::monotonic_time_nanos};
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::*;

use super::IpcPerm;
use crate::{
    StarryError, StarryResult,
    mm::{AddressSpaceMutationOutcome, MmPin, SharedMemoryObject},
    sync::Mutex,
    task::{PidIdentityId, PidNamespaceId, PidSnapshot},
};

/// Data structure describing a shared memory segment.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern, bytemuck::NoUninit)]
pub struct ShmidDs {
    /// operation permission struct
    pub(crate) shm_perm: IpcPerm,
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
    fn new(key: i32, size: usize, mode: __kernel_mode_t, uid: u32, gid: u32) -> Self {
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
            shm_cpid: 0,
            shm_lpid: 0,
            shm_nattch: 0,
            __unused4: 0,
            __unused5: 0,
        }
    }
}

/// A SysV shared-memory segment and its logical attachment records.
pub struct ShmSegment {
    /// Shared memory segment identifier.
    pub shmid: i32,
    /// Number of pages in the shared memory segment.
    pub page_num: usize,
    va_range: BTreeMap<PidIdentityId, Vec<VirtAddrRange>>,
    /// physical pages
    pub phys_pages: Option<Arc<SharedMemoryObject>>,
    /// whether remove on last detach, see shm_ctl
    pub rmid: bool,
    /// Mapping flags used for this shared memory segment.
    pub mapping_flags: MappingFlags,
    /// c type struct, used in shm_ctl
    pub shmid_ds: ShmidDs,
    creator: PidSnapshot,
    last_operator: Option<PidSnapshot>,
    /// IPC namespace ID that owns this segment
    pub ns_id: u64,
}

/// Parameters captured by shmget before publishing a new segment.
pub struct ShmCreation {
    pub key: i32,
    pub size: usize,
    pub shmflg: usize,
    pub creator: PidSnapshot,
    pub uid: u32,
    pub gid: u32,
    pub ns_id: u64,
}

/// Task-context shmat admission, distinct from the VMA's attachment reference.
///
/// The temporary nattch reference follows Linux do_shmat: take it under the
/// registry/object locks, do MM work without either lock, then release it.
pub(crate) struct ShmAttachPreparation {
    segment: Arc<Mutex<ShmSegment>>,
}

pub(crate) struct ShmMapping {
    pub(crate) pages: Arc<SharedMemoryObject>,
    pub(crate) length: usize,
    pub(crate) flags: MappingFlags,
}

impl ShmAttachPreparation {
    pub(crate) fn prepare(shmid: i32, ns_id: u64) -> StarryResult<Self> {
        let manager = SHM_MANAGER.lock();
        let segment = manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(StarryError::InvalidInput)?;
        {
            let mut segment = segment.lock();
            segment.shmid_ds.shm_nattch = segment
                .shmid_ds
                .shm_nattch
                .checked_add(1)
                .ok_or(StarryError::NoMemory)?;
        }
        Ok(Self { segment })
    }

    /// Prepares the backing object before taking the address-space mutex.
    pub(crate) fn mapping(&self) -> StarryResult<ShmMapping> {
        let mut segment = self.segment.lock();
        let length = segment.page_num * PAGE_SIZE_4K;
        if segment.phys_pages.is_none() {
            let pages = Arc::try_new(SharedMemoryObject::allocate(length, PAGE_SIZE_4K)?)
                .map_err(|_| StarryError::NoMemory)?;
            segment.phys_pages = Some(pages);
        }
        Ok(ShmMapping {
            pages: segment.phys_pages.as_ref().unwrap().clone(),
            length,
            flags: segment.mapping_flags,
        })
    }

    /// Records a published mapping while the caller still owns the MM mutex.
    pub(crate) fn record_attachment(
        &self,
        owner: PidIdentityId,
        operator: PidSnapshot,
        range: VirtAddrRange,
    ) {
        let mut manager = SHM_MANAGER.lock();
        let mut segment = self.segment.lock();
        segment.attach_process(owner, operator, range);
        manager.insert_shmid_vaddr(owner, segment.shmid, range.start);
    }
}

impl Drop for ShmAttachPreparation {
    fn drop(&mut self) {
        let mut manager = SHM_MANAGER.lock();
        let mut segment = self.segment.lock();
        assert!(
            segment.shmid_ds.shm_nattch > 0,
            "shmat admission reference underflow"
        );
        segment.shmid_ds.shm_nattch -= 1;
        let shmid = segment.shmid;
        let remove = segment.rmid && segment.attach_count() == 0;
        drop(segment);
        if remove {
            manager.remove_shmid(shmid);
        }
    }
}

impl ShmSegment {
    /// Creates a segment before its ID is inserted into the registry.
    pub fn new(shmid: i32, creation: ShmCreation) -> Self {
        let ShmCreation {
            key,
            size,
            shmflg,
            creator,
            uid,
            gid,
            ns_id,
        } = creation;
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

        ShmSegment {
            shmid,
            page_num: ax_memory_addr::align_up_4k(size) / PAGE_SIZE_4K,
            va_range: BTreeMap::new(),
            phys_pages: None,
            rmid: false,
            mapping_flags,
            shmid_ds: ShmidDs::new(key, size, ipc_mode as __kernel_mode_t, uid, gid),
            last_operator: None,
            creator,
            ns_id,
        }
    }

    pub(crate) fn status(&self, observer: PidNamespaceId) -> ShmidDs {
        let mut status = self.shmid_ds;
        status.shm_cpid = self
            .creator
            .visible_number(observer)
            .map_or(0, |number| number.get() as __kernel_pid_t);
        status.shm_lpid = self
            .last_operator
            .as_ref()
            .and_then(|operator| operator.visible_number(observer))
            .map_or(0, |number| number.get() as __kernel_pid_t);
        status
    }

    /// Validates a `shmget` against an existing segment.
    ///
    /// Mirrors Linux `shm_more_checks()`: the call is rejected with
    /// `EINVAL` only when the requested size is larger than the segment.
    /// The permission bits passed in `shmflg` do not have to match those
    /// used when the segment was created.
    pub fn try_update(&self, size: usize) -> StarryResult<isize> {
        if size as __kernel_size_t > self.shmid_ds.shm_segsz {
            return Err(StarryError::InvalidInput);
        }
        Ok(self.shmid as isize)
    }

    /// Returns VMA/legacy attachment and in-flight shmat admission references.
    pub fn attach_count(&self) -> usize {
        self.shmid_ds.shm_nattch as usize
    }

    /// Returns all virtual address ranges associated with the given Pid.
    pub fn get_addr_ranges(&self, owner: PidIdentityId) -> Vec<VirtAddrRange> {
        self.va_range.get(&owner).cloned().unwrap_or_default()
    }

    /// Returns the virtual address range that starts at the given address.
    pub fn get_addr_range_by_start(
        &self,
        owner: PidIdentityId,
        vaddr: VirtAddr,
    ) -> Option<VirtAddrRange> {
        self.va_range
            .get(&owner)?
            .iter()
            .find(|range| range.start == vaddr)
            .copied()
    }

    /// Attach a process to this segment.
    pub fn attach_process(
        &mut self,
        owner: PidIdentityId,
        operator: PidSnapshot,
        va_range: VirtAddrRange,
    ) {
        self.va_range.entry(owner).or_default().push(va_range);
        self.shmid_ds.shm_nattch = self.shmid_ds.shm_nattch.saturating_add(1);
        self.last_operator = Some(operator);
        self.shmid_ds.shm_atime = monotonic_time_nanos() as __kernel_time_t;
    }

    /// Detach a single attach range from this segment. Returns `false` if the
    /// range was already detached (e.g. by a concurrent clear_proc_shm).
    pub fn detach_process_range(
        &mut self,
        owner: PidIdentityId,
        operator: PidSnapshot,
        vaddr: VirtAddr,
    ) -> bool {
        let Some(ranges) = self.va_range.get_mut(&owner) else {
            return false;
        };
        let Some(index) = ranges.iter().position(|range| range.start == vaddr) else {
            return false;
        };
        ranges.remove(index);
        let empty = ranges.is_empty();
        if empty {
            self.va_range.remove(&owner);
        }
        self.shmid_ds.shm_nattch = self.shmid_ds.shm_nattch.saturating_sub(1);
        self.last_operator = Some(operator);
        self.shmid_ds.shm_dtime = monotonic_time_nanos() as __kernel_time_t;
        true
    }

    /// Detach all attach ranges owned by a process from this segment.
    pub fn detach_process(&mut self, owner: PidIdentityId, operator: PidSnapshot) -> usize {
        let Some(ranges) = self.va_range.remove(&owner) else {
            return 0;
        };
        let attach_count = ranges.len();
        self.shmid_ds.shm_nattch = self
            .shmid_ds
            .shm_nattch
            .saturating_sub(attach_count as __kernel_ulong_t);
        self.last_operator = Some(operator);
        self.shmid_ds.shm_dtime = monotonic_time_nanos() as __kernel_time_t;
        attach_count
    }
}

/// A bidirectional BTreeMap, allowing lookup by key or value.
#[derive(Debug, Clone)]
struct BiBTreeMap<K, V>
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
/// processes. note: this struct do not modify the struct ShmSegment, but only
/// manage the mapping.
pub struct ShmManager {
    /// (key, ns_id) <-> shm_id
    key_shmid: BiBTreeMap<(i32, u64), i32>,
    /// shm_id -> shm_inner
    shmid_inner: BTreeMap<i32, Arc<Mutex<ShmSegment>>>,
    /// process generation -> vaddr -> shm_id
    pid_shmid_vaddr: BTreeMap<PidIdentityId, BTreeMap<VirtAddr, i32>>,
}

impl ShmManager {
    pub(crate) fn mark_for_removal(&mut self, shmid: i32, ns_id: u64) -> StarryResult {
        let segment = self
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(StarryError::InvalidInput)?;
        let mut segment = segment.lock();
        segment.rmid = true;
        segment.shmid_ds.shm_ctime = monotonic_time_nanos() as __kernel_time_t;
        self.make_private(shmid);
        if segment.attach_count() == 0 {
            drop(segment);
            self.remove_shmid(shmid);
        }
        Ok(())
    }
    /// Counts the namespace's allocated segments without exposing registry storage.
    pub(crate) fn segment_count(&self, ns_id: u64) -> usize {
        self.shmid_inner
            .values()
            .filter(|inner| inner.lock().ns_id == ns_id)
            .count()
    }

    /// Returns allocated segment and page counts for SHM_INFO.
    pub(crate) fn namespace_usage(&self, ns_id: u64) -> (i32, u64) {
        let mut used_ids = 0;
        let mut pages = 0;
        for inner in self.shmid_inner.values() {
            let guard = inner.lock();
            if guard.ns_id == ns_id {
                used_ids += 1;
                pages += guard.page_num as u64;
            }
        }
        (used_ids, pages)
    }

    /// Retains the segment at a namespace-local SHM_STAT index.
    pub(crate) fn segment_at(
        &self,
        ns_id: u64,
        index: usize,
    ) -> Option<(i32, Arc<Mutex<ShmSegment>>)> {
        self.shmid_inner
            .iter()
            .filter(|(_, inner)| inner.lock().ns_id == ns_id)
            .nth(index)
            .map(|(id, inner)| (*id, inner.clone()))
    }
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

    /// Returns the shared memory inner structure [`ShmSegment`] associated with
    /// the given shared memory ID, validating that it belongs to the specified
    /// IPC namespace.
    pub fn get_inner_by_shmid(&self, shmid: i32, ns_id: u64) -> Option<Arc<Mutex<ShmSegment>>> {
        self.shmid_inner
            .get(&shmid)
            .filter(|inner| inner.lock().ns_id == ns_id)
            .cloned()
    }

    /// Lookup a shm_inner by shmid without namespace validation. Only for
    /// internal cleanup paths (process exit) where the caller has already
    /// scoped the lookup by pid.
    fn get_inner_by_shmid_unchecked(&self, shmid: i32) -> Option<Arc<Mutex<ShmSegment>>> {
        self.shmid_inner.get(&shmid).cloned()
    }

    /// Returns the shared memory ID associated with the given pid and virtual
    /// address.
    pub fn get_shmid_by_vaddr(&self, owner: PidIdentityId, vaddr: VirtAddr) -> Option<i32> {
        self.pid_shmid_vaddr
            .get(&owner)
            .and_then(|map| map.get(&vaddr))
            .cloned()
    }

    pub(crate) fn get_shmids_by_pid(&self, owner: PidIdentityId) -> Option<Vec<i32>> {
        let map = self.pid_shmid_vaddr.get(&owner)?;
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
    /// structure [`ShmSegment`].
    pub fn insert_shmid_inner(&mut self, shmid: i32, shm_inner: Arc<Mutex<ShmSegment>>) {
        self.shmid_inner.insert(shmid, shm_inner);
    }

    /// Inserts a mapping from a process and shared memory ID to a virtual
    /// address.
    pub fn insert_shmid_vaddr(&mut self, owner: PidIdentityId, shmid: i32, vaddr: VirtAddr) {
        self.pid_shmid_vaddr
            .entry(owner)
            .or_default()
            .insert(vaddr, shmid);
    }

    /// Removes the mapping from a process and shared memory address.
    pub fn remove_shmaddr(&mut self, owner: PidIdentityId, shmaddr: VirtAddr) {
        let mut empty: bool = false;
        if let Some(map) = self.pid_shmid_vaddr.get_mut(&owner) {
            map.remove(&shmaddr);
            empty = map.is_empty();
        }
        if empty {
            self.pid_shmid_vaddr.remove(&owner);
        }
    }

    /// Remove the pid entry from the pid/shmid/vaddr map.
    pub(crate) fn remove_pid(&mut self, owner: PidIdentityId) {
        self.pid_shmid_vaddr.remove(&owner);
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
/// Lock ordering: address-space mutex, then SHM_MANAGER, then ShmSegment.
/// Admission and backing allocation release IPC locks before entering MM;
/// registry/object locks must never be carried into address-space operations.
pub static SHM_MANAGER: Mutex<ShmManager> = Mutex::new(ShmManager::new());

/// Clear all shared memory segments for a process on exit.
///
/// Collects segment info under SHM_MANAGER, drops the lock, unmaps from
/// aspace, then reacquires SHM_MANAGER for bookkeeping. This keeps the
/// lock ordering consistent with sys_shmget (SHM_MANAGER then ShmSegment).
pub fn clear_proc_shm(owner: PidIdentityId, operator: PidSnapshot, aspace: &MmPin) {
    // Collect segments attached to this process.
    let segments: Vec<(i32, Arc<Mutex<ShmSegment>>)> = {
        let shm_manager = SHM_MANAGER.lock();
        let shmids = match shm_manager.get_shmids_by_pid(owner) {
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
        ranges.extend(shm_inner.get_addr_ranges(owner));
    }
    let mut unmap_failed = false;
    if !ranges.is_empty() {
        let mut aspace = aspace.lock();
        for va_range in &ranges {
            match aspace.unmap_outcome(va_range.start, va_range.size()) {
                Ok(AddressSpaceMutationOutcome::Complete)
                | Ok(AddressSpaceMutationOutcome::PublishedPendingTlb(_)) => {}
                Err(error) => {
                    // Do not remove the IPC ownership record when the
                    // address-space transaction did not publish.  The
                    // process is exiting, so leave the range visible for a
                    // repair worker instead of claiming a detach that never
                    // happened.
                    unmap_failed = true;
                    warn!(
                        "shared-memory exit unmap failed at {:#x}+{:#x}: {error}",
                        va_range.start.as_usize(),
                        va_range.size()
                    );
                }
            }
        }
    }

    // Now update the bookkeeping under SHM_MANAGER, then shm_inner.
    if unmap_failed {
        warn!(
            "shared-memory exit cleanup retained IPC ownership for mm identity {:?}",
            owner
        );
        return;
    }
    let mut shm_manager = SHM_MANAGER.lock();
    for (shmid, shm_inner_arc) in segments {
        let mut shm_inner = shm_inner_arc.lock();
        shm_inner.detach_process(owner, operator.clone());
        if shm_inner.rmid && shm_inner.attach_count() == 0 {
            drop(shm_inner);
            shm_manager.remove_shmid(shmid);
        }
    }
    shm_manager.remove_pid(owner);
}

#[cfg(axtest)]
mod tests {
    use super::*;

    #[axtest::axtest]
    fn rmid_preserves_an_admitted_attach_until_cancelled() {
        let namespace = crate::task::new_test_pid_namespace();
        let (identity, _role) = crate::task::new_test_process_identity(&namespace);
        let ns_id = crate::namespace::IpcNamespace::new_root().ns_id;
        let shmid = crate::ipc::next_ipc_id();
        let segment = Arc::new(Mutex::new(ShmSegment::new(
            shmid,
            ShmCreation {
                key: shmid,
                size: PAGE_SIZE_4K,
                shmflg: 0o600,
                creator: identity.snapshot(),
                uid: 0,
                gid: 0,
                ns_id,
            },
        )));
        SHM_MANAGER
            .lock()
            .insert_shmid_inner(shmid, segment.clone());
        let preparation = ShmAttachPreparation::prepare(shmid, ns_id).unwrap();
        SHM_MANAGER.lock().mark_for_removal(shmid, ns_id).unwrap();
        let retained = SHM_MANAGER
            .lock()
            .get_inner_by_shmid(shmid, ns_id)
            .is_some();
        drop(preparation);
        let removed = SHM_MANAGER
            .lock()
            .get_inner_by_shmid(shmid, ns_id)
            .is_none();
        assert!(
            retained,
            "RMID must preserve a segment already admitted by shmat"
        );
        assert!(
            removed,
            "cancelling the last attach preparation must complete RMID"
        );
        // This observer Arc intentionally survives removal: allocation
        // lifetime alone must not keep the segment in the IPC registry.
        assert_eq!(segment.lock().attach_count(), 0);
    }
}
