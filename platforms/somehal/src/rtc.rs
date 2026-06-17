/// Returns the current firmware wall-clock time as Unix epoch nanoseconds.
pub fn epoch_time_nanos() -> Option<u64> {
    someboot::rtc::epoch_time_nanos()
}
