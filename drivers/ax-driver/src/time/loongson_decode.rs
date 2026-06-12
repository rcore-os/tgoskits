use super::datetime::datetime_to_unix_timestamp;

pub(super) fn toy_to_unix_timestamp(toy_high: u32, toy_low: u32) -> Option<u64> {
    let year = 1900 + toy_high as i32;
    let month = extract_bits(toy_low, 26, 6);
    let day = extract_bits(toy_low, 21, 5);
    let hour = extract_bits(toy_low, 16, 5);
    let minute = extract_bits(toy_low, 10, 6);
    let second = extract_bits(toy_low, 4, 6);
    datetime_to_unix_timestamp(year, month, day, hour, minute, second)
}

fn extract_bits(value: u32, start: u32, width: u32) -> u32 {
    (value >> start) & ((1 << width) - 1)
}

#[cfg(test)]
mod tests {
    use super::toy_to_unix_timestamp;

    #[test]
    fn toy_decodes_wall_time_to_unix_timestamp() {
        let toy_high = 125;
        let toy_low = (6 << 26) | (12 << 21) | (8 << 16) | (34 << 10) | (56 << 4);

        let timestamp = toy_to_unix_timestamp(toy_high, toy_low).unwrap();

        assert_eq!(timestamp, 1_749_717_296);
    }

    #[test]
    fn toy_rejects_invalid_zero_timestamp() {
        assert!(toy_to_unix_timestamp(0, 0).is_none());
    }
}
