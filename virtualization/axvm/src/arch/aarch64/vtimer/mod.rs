//! AArch64 architectural timer binding.

mod host_ppi;
mod percpu;
mod state;

pub(super) use host_ppi::ensure_host_timer_ppi;
pub(super) use state::{Aarch64TimerBinding, physical_counter};
