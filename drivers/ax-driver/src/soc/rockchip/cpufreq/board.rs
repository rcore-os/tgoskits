//! Orange Pi 5 Plus limits established by measurements on the board.

/// Maximum delivered CPU rate confirmed for a PVTM voltage grade.
/// A device-tree OPP above this ceiling is not sufficient evidence to use it.
pub(super) const fn verified_maximum_hz(little: bool, grade: u8) -> Option<u64> {
    match (little, grade) {
        // Grade 3 is the robot board's little-cluster bin. The board has been
        // observed running this cluster at 1.8 GHz with a 912.5 mV rail under
        // the robot CI workload, so the ceiling is open for it too; the device
        // tree still restricts the standard bin to 1.608 GHz through
        // `opp-supported-hw`, so the ceiling only bounds what the firmware
        // table may offer.
        (true, 0 | 1 | 3) => Some(1_800_000_000),
        (false, 0) => Some(2_256_000_000),
        (false, 3) => Some(2_352_000_000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::verified_maximum_hz;

    #[test]
    fn unmeasured_grade_rates_are_excluded() {
        assert_eq!(verified_maximum_hz(true, 0), Some(1_800_000_000));
        assert_eq!(verified_maximum_hz(true, 1), Some(1_800_000_000));
        assert_eq!(verified_maximum_hz(true, 3), Some(1_800_000_000));
        assert_eq!(verified_maximum_hz(true, 2), None);
        assert_eq!(verified_maximum_hz(false, 0), Some(2_256_000_000));
        assert_eq!(verified_maximum_hz(false, 3), Some(2_352_000_000));
        assert_eq!(verified_maximum_hz(false, 1), None);
    }
}
