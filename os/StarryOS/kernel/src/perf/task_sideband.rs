//! Process-context side-band publication for task-bound perf events.

use alloc::{string::String, vec::Vec};
use core::sync::atomic::Ordering;

use ax_runtime::hal::paging::MappingFlags;

use super::{
    sideband::{self, Mmap2Info, SidebandTarget},
    task::{PERF_TASK_ACTIVE, sideband_target, visible_tgid, visible_tid},
};
use crate::task::{PidIdentity, TgidNumber, Thread, TidNumber};

// `PROT_*` / `MAP_*` values in PERF_RECORD_MMAP2.
const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const PROT_EXEC: u32 = 4;
const MAP_SHARED: u32 = 1;
const MAP_PRIVATE: u32 = 2;

pub(in crate::perf) fn system_subject(thr: &Thread) -> Option<(TgidNumber, TidNumber)> {
    let observer = thr.active_pid_namespace().id();
    let pid = thr
        .proc_data
        .identity()
        .visible_number_in(observer)
        .map(TgidNumber::from)?;
    let tid = thr
        .pid_identity()
        .visible_number_in(observer)
        .map(TidNumber::from)?;
    Some((pid, tid))
}

/// Snapshots executable file-backed mappings without retaining the address-space
/// lock across ring publication.
fn collect_exec_maps(thr: &Thread) -> Vec<Mmap2Info> {
    let aspace = thr.proc_data.aspace();
    let mm = aspace.lock();
    let mut maps = Vec::new();
    let Ok(records) = mm.vma_inspection_records() else {
        return maps;
    };
    drop(mm);
    for area in records {
        let flags = area.flags();
        if !flags.contains(MappingFlags::EXECUTE) {
            continue;
        }
        let fi = area.file_info();
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
            filename: fi.path.clone(),
        });
    }
    maps
}

/// Emits COMM and executable MMAP2 records after a task commits exec.
pub(crate) fn on_exec_sideband(thr: &Thread) {
    struct WantTarget {
        target: SidebandTarget,
        comm: bool,
        mmap2: bool,
    }

    let mut targets: Vec<WantTarget> = if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        Vec::new()
    } else {
        let counters = thr.perf_context().snapshot();
        counters
            .iter()
            .filter_map(|counter| {
                sideband_target(counter, thr).map(|target| WantTarget {
                    target,
                    comm: counter.wants_comm(),
                    mmap2: counter.wants_mmap2(),
                })
            })
            .collect()
    };
    if let Some((pid, tid)) = system_subject(thr) {
        targets.extend(
            sideband::system_targets(pid, tid)
                .into_iter()
                .map(|target| WantTarget {
                    target: target.target,
                    comm: target.comm,
                    mmap2: target.mmap2,
                }),
        );
    }
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
    let mut targets: Vec<SidebandTarget> = if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        Vec::new()
    } else {
        let counters = thr.perf_context().snapshot();
        counters
            .iter()
            .filter(|counter| counter.wants_mmap2())
            .filter_map(|counter| sideband_target(counter, thr))
            .collect()
    };
    if let Some((pid, tid)) = system_subject(thr) {
        targets.extend(
            sideband::system_targets(pid, tid)
                .into_iter()
                .filter(|target| target.mmap2)
                .map(|target| target.target),
        );
    }
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
pub(crate) fn on_clone_sideband(
    parent_thr: &Thread,
    child_process: &PidIdentity,
    child_thread: &PidIdentity,
) {
    let mut targets: Vec<(SidebandTarget, TgidNumber, TidNumber, TgidNumber, TidNumber)> =
        if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
            Vec::new()
        } else {
            let counters = parent_thr.perf_context().snapshot();
            counters
                .iter()
                .filter(|counter| counter.wants_task())
                .filter_map(|counter| {
                    Some((
                        sideband_target(counter, parent_thr)?,
                        visible_tgid(counter, child_process)?,
                        visible_tid(counter, child_thread)?,
                        visible_tgid(counter, &parent_thr.proc_data.identity())?,
                        visible_tid(counter, &parent_thr.pid_identity())?,
                    ))
                })
                .collect()
        };
    let observer = parent_thr.active_pid_namespace().id();
    if let (Some((parent_pid, parent_tid)), Some(child_pid), Some(child_tid)) = (
        system_subject(parent_thr),
        child_process
            .visible_number_in(observer)
            .map(TgidNumber::from),
        child_thread
            .visible_number_in(observer)
            .map(TidNumber::from),
    ) {
        targets.extend(
            sideband::system_targets(parent_pid, parent_tid)
                .into_iter()
                .filter(|target| target.task)
                .map(|target| (target.target, child_pid, child_tid, parent_pid, parent_tid)),
        );
    }
    for (target, child_pid, child_tid, parent_pid, parent_tid) in &targets {
        sideband::emit_fork(target, *child_pid, *parent_pid, *child_tid, *parent_tid);
    }
}
