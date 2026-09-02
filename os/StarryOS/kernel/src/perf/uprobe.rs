//! Uprobe perf event.
//!
//! `perf_event_open(PERF_TYPE_UPROBE)` carries the target ELF path (`name`), an
//! in-file `offset`, and the target `pid`. We resolve the offset to a live user
//! virtual address by finding the VMA in the target process that is backed by
//! that ELF (`MappingOperation::file_info().path`), then register a uprobe on
//! `vma.start() + offset` in that process' per-process manager.
//!
//! Out-of-line single-step, breakpoint insertion and the eBPF callback are
//! handled by the `kprobe` crate via the user-mode `KprobeAuxiliaryOps` paths
//! in [`crate::kprobe`]; this module only does the address resolution and
//! per-process registration.

use kbpf_basic::perf::{PerfProbeArgs, PerfProbeConfig};
use kprobe::ProbeBuilder;

use super::{
    PerfEventTarget,
    kprobe::{PROBE_CONFIG_ENTRY, PROBE_CONFIG_RETURN, ProbePerfEvent, ProbeTy},
};
use crate::{
    StarryError, StarryResult,
    kprobe::{KprobeAuxiliary, UprobeTargetLease},
    task::{AsThread, get_user_task_by_number},
};

/// Resolve the target ELF's mapped base in the target process and build a
/// uprobe `ProbeBuilder` for `base + offset`.
fn perf_probe_arg_to_uprobe_builder(
    args: &PerfProbeArgs,
    target: PerfEventTarget,
) -> StarryResult<(ProbeBuilder<KprobeAuxiliary>, UprobeTargetLease)> {
    let elf = &args.name;
    let offset = args.offset as usize;

    let task = match target {
        PerfEventTarget::AllTasks => {
            // An all-process shared-library uprobe needs a global
            // file-to-address registry that Starry does not maintain.
            warn!("uprobe: all-process / shared-lib target is unsupported");
            return Err(StarryError::Unsupported);
        }
        PerfEventTarget::Current => ax_task::current().clone(),
        PerfEventTarget::Thread(tid) => get_user_task_by_number(tid)?,
    };
    let target = UprobeTargetLease::register(task.as_thread().pid_identity())?;
    let aspace = task.as_thread().proc_data.pin_aspace()?;
    let mm = aspace.lock();

    let mut virt_base = None;
    for area in mm.vma_inspection_records()? {
        if &area.file_info().path == elf {
            virt_base = Some(area.start());
            break;
        }
    }
    drop(mm);

    let Some(virt_base) = virt_base else {
        warn!("uprobe: ELF {elf} is not mapped in target {target:?}");
        return Err(StarryError::NotFound);
    };

    let virt_addr = virt_base.as_usize() + offset;
    debug!(
        "uprobe: target {target:?} ELF {elf} base {:#x} + offset {:#x} = {virt_addr:#x}",
        virt_base.as_usize(),
        offset
    );

    Ok((
        ProbeBuilder::new()
            .with_symbol(elf.clone())
            .with_symbol_addr(virt_addr)
            .with_offset(0)
            .with_user_mode(target.opaque_id()),
        target,
    ))
}

/// Build a uprobe perf event from `perf_event_open` args.
pub fn perf_event_open_uprobe(
    args: PerfProbeArgs,
    target: PerfEventTarget,
) -> StarryResult<ProbePerfEvent> {
    let (probe, target) = match args.config {
        PerfProbeConfig::Raw(PROBE_CONFIG_ENTRY) => {
            let (builder, target) = perf_probe_arg_to_uprobe_builder(&args, target)?;
            (
                ProbeTy::Uprobe(crate::uprobe::register_uprobe(builder)),
                target,
            )
        }
        PerfProbeConfig::Raw(PROBE_CONFIG_RETURN) => {
            // uretprobe — not implemented for user space yet.
            warn!("uprobe: uretprobe is not yet supported");
            return Err(StarryError::Unsupported);
        }
        other => {
            warn!("uprobe: unsupported perf probe config {other:?}");
            return Err(StarryError::Unsupported);
        }
    };
    Ok(ProbePerfEvent::new_uprobe(args, probe, target))
}
