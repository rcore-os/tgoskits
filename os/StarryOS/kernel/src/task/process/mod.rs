//! Process Management

mod group;
mod session;
mod topology;

pub use group::ProcessGroup;
pub use session::Session;
pub use topology::{Process, ProcessCpuTime, ThreadExit, init_proc};

#[cfg(test)]
pub(super) fn new_isolated_process_for_test(
    identity: alloc::sync::Arc<super::PidIdentity>,
) -> alloc::sync::Arc<Process> {
    Process::new_isolated_for_test(identity)
}
