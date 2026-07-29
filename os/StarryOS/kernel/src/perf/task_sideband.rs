//! Process-context side-band publication for task-bound perf events.

use alloc::{string::String, vec::Vec};
use core::sync::atomic::Ordering;

use ax_runtime::hal::paging::MappingFlags;

use super::{
    sideband::{self, Mmap2Info, SidebandTarget},
    task::{PERF_TASK_ACTIVE, sideband_target},
};
use crate::task::Thread;

// `PROT_*` / `MAP_*` values in PERF_RECORD_MMAP2.
const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const PROT_EXEC: u32 = 4;
const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;

/// Snapshots executable file-backed mappings without retaining the address-space
/// lock across ring publication.
fn collect_exec_maps(thr: &Thread) -> Vec<Mmap2Info> {
    let aspace = thr.proc_data.aspace();
    let mm = aspace.lock();
    let mut maps = Vec::new();
    for area in mm.areas() {
        let flags = area.flags();
        if !flags.contains(MappingFlags::EXECUTE) {
            continue;
        }
        let Ok(fi) = area.backend().file_info() else {
            continue;
        };
        let mut prot = 0u32;
        if flags.contains(MappingFlags::READ) {
            prot |= PROT_READ;
        }
        if flags.contains(MappingFlags::WRITE) {
            prot |= PROT_WRITE;
        }
        prot |= PROT_EXEC;
        maps.push(Mmap2Info {
            addr: area.start().as_usize() as u64,
            len: (area.end().as_usize() - area.start().as_usize()) as u64,
            pgoff: fi.offset.unwrap_or(0),
            maj: 0,
            min: 0,
            ino: fi.inode.unwrap_or(0),
            prot,
            flags: if fi.shared { MAP_SHARED } else { MAP_PRIVATE },
            filename: fi.path,
        });
    }
    maps
}

/// Emits COMM and executable MMAP2 records after a task commits exec.
pub(crate) fn on_exec_sideband(thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let pid = thr.proc_data.proc.pid();
    let tid = thr.tid();

    struct WantTarget {
        target: SidebandTarget,
        comm: bool,
        mmap2: bool,
    }

    let targets: Vec<WantTarget> = {
        let counters = thr.perf_counters().lock();
        counters
            .iter()
            .filter_map(|counter| {
                sideband_target(counter, pid, tid).map(|target| WantTarget {
                    target,
                    comm: counter.wants_comm(),
                    mmap2: counter.wants_mmap2(),
                })
            })
            .collect()
    };
    if targets.is_empty() {
        return;
    }

    let name = crate::task::current_user_task().name();
    for target in &targets {
        if target.comm {
            sideband::emit_comm(&target.target, &name, true);
        }
    }

    if targets.iter().any(|target| target.mmap2) {
        let maps = collect_exec_maps(thr);
        for target in &targets {
            if target.mmap2 {
                for mapping in &maps {
                    sideband::emit_mmap2(&target.target, mapping);
                }
            }
        }
    }
}

/// Emits an MMAP2 record for a newly mapped executable file region.
pub(crate) fn on_mmap_sideband(
    thr: &Thread,
    addr: usize,
    len: usize,
    pgoff: usize,
    prot: u32,
    shared: bool,
    filename: &str,
) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let pid = thr.proc_data.proc.pid();
    let tid = thr.tid();
    let targets: Vec<SidebandTarget> = {
        let counters = thr.perf_counters().lock();
        counters
            .iter()
            .filter(|counter| counter.wants_mmap2())
            .filter_map(|counter| sideband_target(counter, pid, tid))
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let mapping = Mmap2Info {
        addr: addr as u64,
        len: len as u64,
        pgoff: pgoff as u64,
        maj: 0,
        min: 0,
        ino: 0,
        prot,
        flags: if shared { MAP_SHARED } else { MAP_PRIVATE },
        filename: String::from(filename),
    };
    for target in &targets {
        sideband::emit_mmap2(target, &mapping);
    }
}

/// Emits a FORK record into every parent event requesting `attr.task`.
pub(crate) fn on_clone_sideband(parent_thr: &Thread, child_pid: u32, child_tid: u32) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let ppid = parent_thr.proc_data.proc.pid();
    let ptid = parent_thr.tid();
    let targets: Vec<SidebandTarget> = {
        let counters = parent_thr.perf_counters().lock();
        counters
            .iter()
            .filter(|counter| counter.wants_task())
            .filter_map(|counter| sideband_target(counter, ppid, ptid))
            .collect()
    };
    for target in &targets {
        sideband::emit_fork(target, child_pid, ppid, child_tid, ptid);
    }
}
