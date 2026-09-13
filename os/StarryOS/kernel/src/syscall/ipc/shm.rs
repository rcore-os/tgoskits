use alloc::sync::Arc;

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use ax_runtime::hal::{paging::MappingFlags, time::monotonic_time_nanos};
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::*;

use super::{
    IPC_CREAT, IPC_EXCL, IPC_INFO, IPC_PRIVATE, IPC_RMID, IPC_SET, IPC_STAT, SHM_INFO, SHM_STAT,
};
use crate::{
    StarryError,
    ipc::{
        has_ipc_permission, next_ipc_id,
        shm::{SHM_MANAGER, ShmAttachPreparation, ShmCreation, ShmSegment, ShmidDs},
    },
    mm::{AddressSpaceMutationOutcome, MappingOperation, UserPtr, VmMutPtr, VmPtr},
    sync::Mutex,
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

pub fn sys_shmget(
    current: &crate::task::UserTaskRef,
    key: i32,
    size: usize,
    shmflg: usize,
) -> crate::StarryResult<isize> {
    let curr = current;
    let thread = curr.as_thread();
    let operator = thread.proc_data.identity().snapshot();
    let cred = thread.cred();
    let ns_id = thread.proc_data.namespace_snapshot().ipc_ns.lock().ns_id;
    let mut shm_manager = SHM_MANAGER.lock();

    if key != IPC_PRIVATE {
        // A segment already exists for this key.
        if let Some(shmid) = shm_manager.get_shmid_by_key(key, ns_id) {
            // IPC_CREAT | IPC_EXCL requires the creation to fail when the
            // segment is already present. See Linux ipcget_public().
            if shmflg & IPC_CREAT as usize != 0 && shmflg & IPC_EXCL as usize != 0 {
                return Err(StarryError::AlreadyExists);
            }
            let shm_inner = shm_manager
                .get_inner_by_shmid(shmid, ns_id)
                .ok_or(StarryError::NotFound)?;
            let shm_inner = shm_inner.lock();
            return shm_inner.try_update(size);
        }

        // No segment exists for this key: create one only when IPC_CREAT
        // is requested, otherwise the lookup fails with ENOENT.
        if shmflg & IPC_CREAT as usize == 0 {
            return Err(StarryError::NotFound);
        }
    }

    // Creating a new segment: its page-rounded size must be non-zero.
    let page_num = ax_memory_addr::align_up_4k(size) / PAGE_SIZE_4K;
    if page_num == 0 {
        return Err(StarryError::InvalidInput);
    }

    // Create a new shm_inner
    let shmid = next_ipc_id();
    let shm_inner = Arc::new(Mutex::new(ShmSegment::new(
        shmid,
        ShmCreation {
            key,
            size,
            shmflg,
            creator: operator,
            uid: cred.euid,
            gid: cred.egid,
            ns_id,
        },
    )));
    shm_manager.insert_key_shmid(key, ns_id, shmid);
    shm_manager.insert_shmid_inner(shmid, shm_inner);

    Ok(shmid as isize)
}

pub fn sys_shmat(
    current: &crate::task::UserTaskRef,
    shmid: i32,
    addr: usize,
    shmflg: u32,
) -> crate::StarryResult<isize> {
    let shm_flg = ShmAtFlags::from_bits_truncate(shmflg);

    let curr = current;
    let proc_data = &curr.as_thread().proc_data;
    let pid = proc_data.proc.pid();
    let owner = proc_data.identity().id();
    let operator = proc_data.identity().snapshot();

    info!("shmat pid={pid} shmid={shmid} enter");

    let ns_id = proc_data.namespace_snapshot().ipc_ns.lock().ns_id;
    let preparation = ShmAttachPreparation::prepare(shmid, ns_id)?;
    let mapping = preparation.mapping()?;
    let mut mapping_flags = mapping.flags;
    if shm_flg.contains(ShmAtFlags::SHM_RDONLY) {
        mapping_flags.remove(MappingFlags::WRITE);
    }

    // SHM_RND and SHM_REMAP retain their existing unsupported behavior here.
    let start_aligned = ax_memory_addr::align_down_4k(addr);
    let length = mapping.length;
    let aspace_arc = proc_data.pin_aspace()?;
    let (start_addr, outcome) = {
        let mut aspace = aspace_arc.lock();
        let range = VirtAddrRange::new(aspace.base(), aspace.end());
        let start_addr = aspace
            .find_free_area(VirtAddr::from(start_aligned), length, range, PAGE_SIZE_4K)
            .or_else(|| aspace.find_free_area(aspace.base(), length, range, PAGE_SIZE_4K))
            .ok_or(StarryError::NoMemory)?;
        let backend = MappingOperation::new_shared(start_addr, mapping.pages);
        let outcome = aspace.map_outcome(start_addr, length, mapping_flags, false, backend)?;
        // PublishedPendingTlb already owns a VMA; it must retain its attach
        // reference even if the syscall reports the pending TLB error.
        preparation.record_attachment(
            owner,
            operator,
            VirtAddrRange::from_start_size(start_addr, length),
        );
        (start_addr, outcome)
    };
    // Linux releases its temporary nattch reference after mmap_write_unlock.
    drop(preparation);
    match outcome {
        AddressSpaceMutationOutcome::Complete => Ok(start_addr.as_usize() as isize),
        AddressSpaceMutationOutcome::PublishedPendingTlb(error) => Err(error),
    }
}

pub fn sys_shmctl(
    current: &crate::task::UserTaskRef,
    shmid: i32,
    cmd: u32,
    buf: UserPtr<ShmidDs>,
) -> crate::StarryResult<isize> {
    let cmd = cmd as i32;

    let curr = current;
    let thread = curr.as_thread();
    let cred = thread.cred();
    let ns_id = thread.proc_data.nsproxy.lock().ipc_ns.lock().ns_id;
    let pid_observer = thread.active_pid_namespace().id();

    // IPC_INFO: system-wide shared memory limits (no segment lookup).
    if cmd == IPC_INFO {
        let ns_count = SHM_MANAGER.lock().segment_count(ns_id);
        let info = ShmInfo64 {
            shmmax: usize::MAX as u64,
            shmmin: 1,
            shmmni: 4096,
            shmseg: 4096,
            shmall: usize::MAX as u64 / PAGE_SIZE_4K as u64,
        };
        let ptr = buf.as_ptr() as *mut ShmInfo64;
        ptr.vm_write(current, info)?;
        let max_idx = ns_count.saturating_sub(1) as isize;
        return Ok(max_idx);
    }

    // SHM_INFO: shared memory usage statistics for this namespace.
    if cmd == SHM_INFO {
        let (used_ids, shm_tot) = SHM_MANAGER.lock().namespace_usage(ns_id);
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
        ptr.vm_write(current, info)?;
        let max_idx = used_ids.saturating_sub(1) as isize;
        return Ok(max_idx);
    }

    // SHM_STAT: return the shmid_ds for the shmid at the given index,
    // counting only segments in this namespace.
    if cmd == SHM_STAT {
        let (actual_shmid, shmid_ds) = {
            let shm_manager = SHM_MANAGER.lock();
            let (actual_shmid, inner) = shm_manager
                .segment_at(ns_id, shmid as usize)
                .ok_or(StarryError::InvalidInput)?;
            let guard = inner.lock();
            if !has_ipc_permission(&guard.shmid_ds.shm_perm, cred.euid, cred.egid, false) {
                return Err(StarryError::PermissionDenied);
            }
            (actual_shmid, guard.status(pid_observer))
        };
        buf.as_ptr().vm_write(current, shmid_ds)?;
        return Ok(actual_shmid as isize);
    }

    if cmd == IPC_RMID {
        // If no processes are attached, destroy the segment immediately.
        // Otherwise mark it for deferred destruction and remove the key
        // mapping so future shmget() calls won't find it. See Linux
        // do_shm_rmid() in ipc/shm.c.
        SHM_MANAGER.lock().mark_for_removal(shmid, ns_id)?;
        return Ok(0);
    }

    // Copy IPC_SET input before taking shared-memory metadata locks. User
    // memory access can fault and sleep, so it must not retain these locks.
    let requested = if cmd == IPC_SET {
        Some((buf.as_ptr() as *const ShmidDs).vm_read(current)?)
    } else {
        None
    };

    // IPC_SET and IPC_STAT only need shm_inner.
    let shm_inner_arc = {
        let shm_manager = SHM_MANAGER.lock();
        shm_manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(StarryError::InvalidInput)?
    };
    let mut shm_inner = shm_inner_arc.lock();

    if let Some(requested) = requested {
        shm_inner
            .shmid_ds
            .shm_perm
            .update_from_user(&requested.shm_perm);
        shm_inner.shmid_ds.shm_ctime = monotonic_time_nanos() as __kernel_time_t;
        return Ok(0);
    }
    if cmd != IPC_STAT {
        return Err(StarryError::InvalidInput);
    }

    let output = (!buf.is_null()).then(|| shm_inner.status(pid_observer));
    drop(shm_inner);
    if let Some(output) = output {
        buf.as_ptr().vm_write(current, output)?;
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
pub fn sys_shmdt(current: &crate::task::UserTaskRef, shmaddr: usize) -> crate::StarryResult<isize> {
    let shmaddr = VirtAddr::from(shmaddr);

    let curr = current;
    let proc_data = &curr.as_thread().proc_data;
    let pid = proc_data.proc.pid();
    let owner = proc_data.identity().id();
    let operator = proc_data.identity().snapshot();

    info!("shmdt pid={pid} addr={shmaddr:?} enter");

    // Look up shmid and grab the inner Arc while holding SHM_MANAGER.
    let (shmid, shm_inner_arc) = {
        let shm_manager = SHM_MANAGER.lock();
        let ns_id = proc_data.namespace_snapshot().ipc_ns.lock().ns_id;
        let shmid = shm_manager
            .get_shmid_by_vaddr(owner, shmaddr)
            .ok_or(StarryError::InvalidInput)?;
        let shm_inner_arc = shm_manager
            .get_inner_by_shmid(shmid, ns_id)
            .ok_or(StarryError::InvalidInput)?;
        (shmid, shm_inner_arc)
    };

    // Snapshot the mapped range for this process.
    let va_range = {
        info!("shmdt pid={pid} lock shm_inner for range");
        let shm_inner = shm_inner_arc.lock();
        shm_inner
            .get_addr_range_by_start(owner, shmaddr)
            .ok_or(StarryError::InvalidInput)?
    };

    // Unmap while only holding the aspace lock.
    let pending_tlb_error = {
        info!("shmdt pid={pid} lock aspace for unmap");
        let aspace_arc = proc_data.pin_aspace()?;
        let mut aspace = aspace_arc.lock();
        match aspace.unmap_outcome(va_range.start, va_range.size())? {
            AddressSpaceMutationOutcome::Complete => None,
            AddressSpaceMutationOutcome::PublishedPendingTlb(error) => Some(error),
        }
    };

    // Reacquire SHM_MANAGER then shm_inner for bookkeeping, matching
    // the global lock ordering.
    info!("shmdt pid={pid} lock shm_manager for bookkeeping");
    let mut shm_manager = SHM_MANAGER.lock();
    shm_manager.remove_shmaddr(owner, shmaddr);
    let mut shm_inner = shm_inner_arc.lock();

    // detach_process_range returns false if clear_proc_shm already detached
    // this pid (race during process exit).
    if shm_inner.detach_process_range(owner, operator, shmaddr)
        && shm_inner.rmid
        && shm_inner.attach_count() == 0
    {
        drop(shm_inner);
        shm_manager.remove_shmid(shmid);
    }

    match pending_tlb_error {
        Some(error) => Err(error),
        None => Ok(0),
    }
}
