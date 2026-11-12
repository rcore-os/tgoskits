//! Orange Pi 5 Plus limits established by measurements on the board.

/// Maximum delivered CPU rate confirmed for a PVTM voltage grade.
/// A device-tree OPP above this ceiling is not sufficient evidence to use it.
pub(super) const fn verified_maximum_hz(little: bool, grade: u8) -> Option<u64> {
    match (little, grade) {
        (true, 0 | 1) => Some(1_800_000_000),
        (false, 0) => Some(2_256_000_000),
        (false, 3) => Some(2_352_000_000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    use super::{super as cpufreq, verified_maximum_hz};

    #[test]
    fn unmeasured_grade_rates_are_excluded() {
        assert_eq!(verified_maximum_hz(true, 0), Some(1_800_000_000));
        assert_eq!(verified_maximum_hz(true, 1), Some(1_800_000_000));
        assert_eq!(verified_maximum_hz(false, 0), Some(2_256_000_000));
        assert_eq!(verified_maximum_hz(false, 3), Some(2_352_000_000));
        assert_eq!(verified_maximum_hz(false, 1), None);

        // Only this host test changes these boot/policy flags. The internal
        // ring remains available for DSU accounting, but is not a public OPP.
        let domain = cpufreq::FrequencyDomain::Big1;
        let index = domain.index();
        let was_ready = cpufreq::GOV_READY.swap(true, Ordering::AcqRel);
        let was_domain_ready = cpufreq::DOMAIN_READY[index].swap(true, Ordering::AcqRel);
        let was_fallback = cpufreq::LIMITED_FALLBACK[index].swap(true, Ordering::AcqRel);
        let previous_index = cpufreq::IDX[index].swap(0, Ordering::AcqRel);
        let previous_floor = cpufreq::FLOOR_IDX[index].swap(0, Ordering::AcqRel);
        let results = [
            (
                "available OPPs",
                cpufreq::available_opps(domain).map(|_| ()),
            ),
            ("all OPPs", cpufreq::all_opps(domain).map(|_| ())),
            ("current OPP", cpufreq::current_opp(domain).map(|_| ())),
            ("limits", cpufreq::limits(domain).map(|_| ())),
            // An unavailable domain rejects before validating a request;
            // this value cannot reach a hardware write in the old code.
            ("set frequency", cpufreq::set_frequency(domain, u64::MAX)),
        ];
        cpufreq::FLOOR_IDX[index].store(previous_floor, Ordering::Release);
        cpufreq::IDX[index].store(previous_index, Ordering::Release);
        cpufreq::LIMITED_FALLBACK[index].store(was_fallback, Ordering::Release);
        cpufreq::DOMAIN_READY[index].store(was_domain_ready, Ordering::Release);
        cpufreq::GOV_READY.store(was_ready, Ordering::Release);

        for (operation, result) in results {
            assert_eq!(
                result,
                Err(cpufreq::FrequencyError::NotReady),
                "{operation}"
            );
        }
    }
}
