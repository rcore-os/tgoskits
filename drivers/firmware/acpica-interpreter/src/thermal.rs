//! ACPI decikelvin conversion and critical-trip decisions, no cooling policy.
use crate::{BAD_PARAMETER, Status};
/// Match Linux's ACPI thermal offset heuristic for integer-decikelvin firmware.
pub fn millicelsius(raw: u64, critical: Option<u64>) -> Result<i64, Status> {
    if raw == 0 || raw == u64::from(u32::MAX) {
        return Err(BAD_PARAMETER);
    }
    let scaled = i64::try_from(raw)
        .ok()
        .and_then(|v| v.checked_mul(100))
        .ok_or(BAD_PARAMETER)?;
    let offset = if critical.is_some_and(|v| v % 5 == 1) {
        273100
    } else {
        273200
    };
    scaled.checked_sub(offset).ok_or(BAD_PARAMETER)
}
pub fn critical_reached(current: u64, critical: u64) -> bool {
    // Policy and sysfs share validity: an overflow/sentinel is not a real trip.
    millicelsius(current, Some(critical)).is_ok()
        && millicelsius(critical, Some(critical)).is_ok()
        && critical > 2732
        && current >= critical
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn conversion_and_invalid_values() {
        assert_eq!(millicelsius(3000, Some(3501)), Ok(26900));
        assert_eq!(millicelsius(3000, Some(3500)), Ok(26800));
        assert!(millicelsius(0, None).is_err());
        assert!(millicelsius(u64::MAX, None).is_err());
    }
    #[test]
    fn exact_critical_boundary() {
        assert!(!critical_reached(3499, 3500));
        assert!(critical_reached(3500, 3500));
        assert!(critical_reached(3501, 3500));
        assert!(!critical_reached(3500, 0));
        assert!(!critical_reached(u32::MAX.into(), 3500));
        assert!(!critical_reached(u64::MAX, 3500));
        assert!(!critical_reached(u64::MAX, u64::MAX));
    }
}
