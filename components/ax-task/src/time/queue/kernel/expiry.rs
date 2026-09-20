//! Expiry under the owning scheduler transaction.

use super::*;

impl KernelTimerQueue {
    pub(crate) fn expire_due_soft(
        &mut self,
        now: MonotonicInstant,
        budget: usize,
    ) -> KernelTimerExpireBatch {
        let mut expired = 0;
        while expired < budget {
            let Some(index) = self.next_active_index(false) else {
                break;
            };
            if !now.reached(self.active[index].deadline()) {
                break;
            }
            let mut entry = self.active.remove(index);
            entry.expire(now);
            self.expired.push(entry);
            expired += 1;
        }
        KernelTimerExpireBatch {
            expired,
            pending: self.has_due_soft(now),
        }
    }

    pub(crate) fn claim_due_hard(&mut self, now: MonotonicInstant) -> Option<KernelTimerExecution> {
        let index = self.next_active_index(true)?;
        if !now.reached(self.active[index].deadline()) {
            return None;
        }
        let mut entry = self.active.remove(index);
        entry.expire(now);
        self.executing.push(ExecutingKernelTimer {
            identity: entry.identity(),
            hard: entry.is_hard(),
            disposition: ExecutingKernelTimerDisposition::Continue,
        });
        Some(KernelTimerExecution { entry })
    }

    pub(crate) fn claim_expired(&mut self) -> Option<KernelTimerExecution> {
        if self.expired.is_empty() {
            return None;
        }
        let entry = self.expired.remove(0);
        self.executing.push(ExecutingKernelTimer {
            identity: entry.identity(),
            hard: entry.is_hard(),
            disposition: ExecutingKernelTimerDisposition::Continue,
        });
        Some(KernelTimerExecution { entry })
    }
}
