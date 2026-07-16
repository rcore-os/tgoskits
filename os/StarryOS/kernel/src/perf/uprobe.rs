//! Uprobe perf event.
//!
//! `perf_event_open(PERF_TYPE_UPROBE)` carries the target ELF path (`name`), an
//! in-file `offset`, and the target `pid`. We resolve the offset to a live user
//! virtual address by finding the VMA in the target process that is backed by
//! that ELF (`Backend::file_info().path`), then register a uprobe on
//! `vma.start() + offset` in that process' per-process manager.
//!
//! Out-of-line single-step, breakpoint insertion and the eBPF callback are
//! handled by the `kprobe` crate via the user-mode `KprobeAuxiliaryOps` paths
//! in [`crate::kprobe`]; this module only does the address resolution and
//! per-process registration.

use ax_errno::{AxError, AxResult};
use kbpf_basic::perf::{PerfProbeArgs, PerfProbeConfig};
use kprobe::ProbeBuilder;

use super::kprobe::{PROBE_CONFIG_ENTRY, PROBE_CONFIG_RETURN, ProbePerfEvent, ProbeTy};
use crate::{kprobe::KprobeAuxiliary, task::get_task};

/// Resolve the target ELF's mapped base in the target process and build a
/// uprobe `ProbeBuilder` for `base + offset`.
fn perf_probe_arg_to_uprobe_builder(
    args: &PerfProbeArgs,
) -> AxResult<ProbeBuilder<KprobeAuxiliary>> {
    let elf = &args.name;
    let offset = args.offset as usize;
    let pid = args.pid;

    if pid < 0 {
        // pid == -1 means "all processes" (e.g. a shared-library uprobe). That
        // needs a global file→address registry we do not maintain.
        warn!("uprobe: pid == -1 (all-process / shared-lib uprobe) is unsupported");
        return Err(AxError::Unsupported);
    }

    let task = get_task(pid as _)?;
    let aspace = task.as_thread().proc_data.aspace();
    let mm = aspace.lock();

    let mut virt_base = None;
    for area in mm.areas() {
        if let Ok(info) = area.backend().file_info()
            && &info.path == elf
        {
            virt_base = Some(area.start());
            break;
        }
    }
    drop(mm);

    let Some(virt_base) = virt_base else {
        warn!("uprobe: ELF {elf} is not mapped in pid {pid}");
        return Err(AxError::NotFound);
    };

    let virt_addr = virt_base.as_usize() + offset;
    debug!(
        "uprobe: pid {pid} ELF {elf} base {:#x} + offset {:#x} = {virt_addr:#x}",
        virt_base.as_usize(),
        offset
    );

    Ok(ProbeBuilder::new()
        .with_symbol(elf.clone())
        .with_symbol_addr(virt_addr)
        .with_offset(0)
        .with_user_mode(pid))
}

/// Build a uprobe perf event from `perf_event_open` args.
pub fn perf_event_open_uprobe(args: PerfProbeArgs) -> AxResult<ProbePerfEvent> {
    let probe = match args.config {
        PerfProbeConfig::Raw(PROBE_CONFIG_ENTRY) => {
            let builder = perf_probe_arg_to_uprobe_builder(&args)?;
            ProbeTy::Uprobe(crate::uprobe::register_uprobe(builder))
        }
        PerfProbeConfig::Raw(PROBE_CONFIG_RETURN) => {
            // uretprobe — not implemented for user space yet.
            warn!("uprobe: uretprobe is not yet supported");
            return Err(AxError::Unsupported);
        }
        other => {
            warn!("uprobe: unsupported perf probe config {other:?}");
            return Err(AxError::Unsupported);
        }
    };
    Ok(ProbePerfEvent::new(args, probe))
}
