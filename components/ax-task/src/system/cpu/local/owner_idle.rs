//! Remote-work and atomic idle-polling facade.

use super::*;

impl CpuLocal {
    /// Reports pending remote work before idle or scheduler exit.
    pub(crate) fn has_remote_work(&self) -> bool {
        self.remote.has_remote_work()
    }

    /// Publishes the idle/polling state and performs the final WFI recheck.
    pub(crate) fn prepare_idle_wait(&self) -> bool {
        self.remote.prepare_idle_wait()
    }

    /// Clears idle/polling publication after WFI returns.
    pub(crate) fn finish_idle_wait(&self) {
        self.remote.finish_idle_wait();
    }

    /// Returns whether this CPU is between idle publication and WFI completion.
    pub fn is_idle_polling(&self) -> bool {
        self.remote.is_idle_polling()
    }
}
