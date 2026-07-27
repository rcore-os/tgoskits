//! Root-domain SCHED_DEADLINE bandwidth admission.

use crate::{DeadlinePolicy, TaskError};

pub(crate) const DEADLINE_UTILIZATION_SCALE: u64 = 1_000_000_000;

/// Admission accounting for one online root domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineAdmission {
    cap_percent: u8,
    online_cpus: usize,
    reserved_scaled: u128,
}

impl DeadlineAdmission {
    /// Creates empty admission state.
    pub const fn new(cap_percent: u8) -> Self {
        Self {
            cap_percent,
            online_cpus: 0,
            reserved_scaled: 0,
        }
    }

    /// Updates the number of CPUs belonging to the online root domain.
    pub const fn set_online_cpus(&mut self, online_cpus: usize) {
        self.online_cpus = online_cpus;
    }

    /// Reserves utilization for a Deadline policy.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::DeadlineAdmission`] if the reservation exceeds the
    /// configured root-domain cap.
    pub fn reserve(&mut self, policy: DeadlinePolicy) -> Result<u128, TaskError> {
        let utilization = Self::utilization(policy);
        self.reserve_utilization(utilization)?;
        Ok(utilization)
    }

    pub(crate) fn reserve_utilization(&mut self, utilization: u128) -> Result<(), TaskError> {
        let next = self.reserved_scaled.saturating_add(utilization);
        if next > self.capacity_scaled() {
            return Err(TaskError::DeadlineAdmission);
        }
        self.reserved_scaled = next;
        Ok(())
    }

    pub(crate) const fn utilization(policy: DeadlinePolicy) -> u128 {
        scaled_utilization(policy)
    }

    /// Releases a value returned by [`Self::reserve`].
    pub fn release(&mut self, utilization: u128) {
        self.reserved_scaled = self.reserved_scaled.saturating_sub(utilization);
    }

    /// Returns the currently reserved fixed-point utilization.
    pub const fn reserved_scaled(self) -> u128 {
        self.reserved_scaled
    }

    /// Returns the fixed-point capacity of the online root domain.
    pub const fn capacity_scaled(self) -> u128 {
        (self.online_cpus as u128)
            .saturating_mul(self.cap_percent as u128)
            .saturating_mul(DEADLINE_UTILIZATION_SCALE as u128)
            / 100
    }
}

const fn scaled_utilization(policy: DeadlinePolicy) -> u128 {
    let numerator =
        (policy.runtime_ns() as u128).saturating_mul(DEADLINE_UTILIZATION_SCALE as u128);
    let period = policy.period_ns() as u128;
    numerator.saturating_add(period - 1) / period
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
    }
}
