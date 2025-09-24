use alloc::sync::Arc;

use axerrno::{AxError, AxResult};
use axhal::{
    paging::{MappingFlags, PageSize},
    time::monotonic_time_nanos,
};
use axmm::backend::{Backend, SharedPages};
use axsync::Mutex;
use axtask::current;
use linux_raw_sys::general::*;
use memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};
use starry_core::{
    shm::{SHM_MANAGER, ShmInner, ShmidDs},
    task::AsThread,
};

use super::next_ipc_id;
use crate::mm::{UserPtr, nullable};

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
const IPC_PRIVATE: i32 = 0;

const IPC_RMID: u32 = 0;

const IPC_SET: u32 = 1;

const IPC_STAT: u32 = 2;

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
        )
        .or_else(|| {
            aspace.find_free_area(
                aspace.base(),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
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
