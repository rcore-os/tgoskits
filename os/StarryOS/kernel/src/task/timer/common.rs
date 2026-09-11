use super::*;

pub(super) fn time_value_from_nanos(nanos: u64) -> TimeValue {
    let secs = nanos / NANOS_PER_SEC;
    let nsecs = nanos - secs * NANOS_PER_SEC;
    TimeValue::new(secs, nsecs as u32)
}
