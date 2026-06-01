//! Uprobe perf event. Tracks the kernel-side
//! [`crate::uprobe::KernelUprobe`] registration; the source resolves the
//! ELF base address by walking the target process' VMAs, which requires
//! the uprobe_manager / uprobe_point_list fields on `ProcessData` and an
//! `AddrSpace::memoryset()` accessor that tgoskits has not introduced yet
//! (post-#805 / #673 baselines lack both).
//!
//! Until that infrastructure lands (planned for a follow-up that extends
//! `ProcessData`), opening a uprobe perf event surfaces
//! [`AxError::Unsupported`] rather than panicking the kernel. The
//! placeholder still threads the `PerfProbeArgs` through so the upper
//! layers compile end-to-end.

use ax_errno::{AxError, AxResult};
use kbpf_basic::perf::PerfProbeArgs;

use super::kprobe::ProbePerfEvent;

/// Build a uprobe perf event from `perf_event_open` args. Always returns
/// `Unsupported` for now; see module docs.
pub fn perf_event_open_uprobe(_args: PerfProbeArgs) -> AxResult<ProbePerfEvent> {
    warn!("perf_event_open(PERF_TYPE_UPROBE) is not yet supported on tgoskits");
    Err(AxError::Unsupported)
}
