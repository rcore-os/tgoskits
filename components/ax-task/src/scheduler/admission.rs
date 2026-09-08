//! Root-domain SCHED_DEADLINE bandwidth admission.

use crate::{DeadlinePolicy, TaskError};

pub(crate) const DEADLINE_UTILIZATION_SCALE: u64 = 1_000_000_000;

/// Admission accounting for one online root domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeadlineAdmission {
    cap_percent: u8,
    online_cpus: u32,
    reserved_scaled: u64,
}

impl DeadlineAdmission {
    /// Creates empty admission state.
    pub(crate) const fn new(cap_percent: u8) -> Self {
        Self {
            cap_percent,
            online_cpus: 0,
            reserved_scaled: 0,
        }
    }

    /// Updates the number of CPUs belonging to the online root domain.
    pub(crate) const fn set_online_cpus(&mut self, online_cpus: u32) {
        self.online_cpus = online_cpus;
    }

    /// Reserves utilization for a Deadline policy.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::DeadlineAdmission`] if the reservation exceeds the
    /// configured root-domain cap.
    #[cfg(test)]
    fn reserve(&mut self, policy: DeadlinePolicy) -> Result<u64, TaskError> {
        let utilization = Self::utilization(policy);
        self.reserve_utilization(utilization)?;
        Ok(utilization)
    }

    pub(crate) fn reserve_utilization(&mut self, utilization: u64) -> Result<(), TaskError> {
        self.replace_utilization(0, utilization)
    }

    pub(crate) fn replace_utilization(
        &mut self,
        old_utilization: u64,
        new_utilization: u64,
    ) -> Result<(), TaskError> {
        let without_old = self
            .reserved_scaled
            .checked_sub(old_utilization)
            .ok_or(TaskError::InvalidConfiguration)?;
        let next = without_old
            .checked_add(new_utilization)
            .ok_or(TaskError::DeadlineAdmission)?;
        if next > self.capacity_scaled() {
            return Err(TaskError::DeadlineAdmission);
        }
        self.reserved_scaled = next;
        Ok(())
    }

    pub(crate) fn utilization(policy: DeadlinePolicy) -> u64 {
        scaled_utilization(policy)
    }

    /// Releases a value returned by [`Self::reserve`].
    #[cfg(test)]
    fn release(&mut self, utilization: u64) -> Result<(), TaskError> {
        self.replace_utilization(utilization, 0)
    }

    /// Returns the currently reserved fixed-point utilization.
    pub const fn reserved_scaled(self) -> u64 {
        self.reserved_scaled
    }

    /// Returns the fixed-point capacity of the online root domain.
    pub const fn capacity_scaled(self) -> u64 {
        (self.online_cpus as u64) * (self.cap_percent as u64) * DEADLINE_UTILIZATION_SCALE / 100
    }
}

fn scaled_utilization(policy: DeadlinePolicy) -> u64 {
    let numerator = (policy.runtime_ns() as u128) * (DEADLINE_UTILIZATION_SCALE as u128);
    let period = policy.period_ns() as u128;
    u64::try_from(numerator.div_ceil(period))
        .expect("a validated Deadline reservation cannot exceed one CPU")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeadlineFlags;

    #[test]
    fn enforces_the_root_domain_cap() {
        let mut admission = DeadlineAdmission::new(95);
        admission.set_online_cpus(1);
        let half = DeadlinePolicy::new(5, 10, 10, DeadlineFlags::NONE).unwrap();
        let first = admission.reserve(half).unwrap();
        assert_eq!(first, 500_000_000);
        assert_eq!(admission.reserve(half), Err(TaskError::DeadlineAdmission));
        admission.release(first).unwrap();
        assert_eq!(admission.release(1), Err(TaskError::InvalidConfiguration));
        assert_eq!(admission.reserved_scaled(), 0);
    }
}
