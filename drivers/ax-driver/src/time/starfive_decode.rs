use super::datetime::datetime_to_unix_timestamp;

const RTC_SEC_SHIFT: u32 = 0;
const RTC_SEC_MASK: u32 = 0x7f;
const RTC_MIN_SHIFT: u32 = 7;
const RTC_MIN_MASK: u32 = 0x7f;
const RTC_HOUR_SHIFT: u32 = 14;
const RTC_HOUR_MASK: u32 = 0x7f;
const RTC_DAY_SHIFT: u32 = 0;
const RTC_DAY_MASK: u32 = 0x3f;
const RTC_MONTH_SHIFT: u32 = 6;
const RTC_MONTH_MASK: u32 = 0x1f;
const RTC_YEAR_SHIFT: u32 = 11;
const RTC_YEAR_MASK: u32 = 0xff;

pub(super) fn decode_rtc_datetime(time_reg: u32, date_reg: u32) -> Option<u64> {
    let second = (time_reg >> RTC_SEC_SHIFT) & RTC_SEC_MASK;
    let minute = (time_reg >> RTC_MIN_SHIFT) & RTC_MIN_MASK;
    let hour = (time_reg >> RTC_HOUR_SHIFT) & RTC_HOUR_MASK;
    let day = (date_reg >> RTC_DAY_SHIFT) & RTC_DAY_MASK;
    let month = (date_reg >> RTC_MONTH_SHIFT) & RTC_MONTH_MASK;
    let year_since_2000 = (date_reg >> RTC_YEAR_SHIFT) & RTC_YEAR_MASK;

    if !(1..=99).contains(&year_since_2000) {
        return None;
    }

    let year = 2000 + year_since_2000 as i32;
    datetime_to_unix_timestamp(year, month, day, hour, minute, second)
}

#[cfg(test)]
mod tests {
    use super::decode_rtc_datetime;

    fn time_reg(hour: u32, minute: u32, second: u32) -> u32 {
        second | (minute << 7) | (hour << 14)
    }

    fn date_reg(year_since_2000: u32, month: u32, day: u32) -> u32 {
        day | (month << 6) | (year_since_2000 << 11)
    }

    #[test]
    fn decodes_valid_wall_clock() {
        assert_eq!(
            decode_rtc_datetime(time_reg(8, 34, 56), date_reg(25, 6, 12)),
            Some(1_749_717_296)
        );
    }

    #[test]
    fn rejects_invalid_month_and_day() {
        assert_eq!(
            decode_rtc_datetime(time_reg(8, 34, 56), date_reg(25, 13, 12)),
            None
        );
        assert_eq!(
            decode_rtc_datetime(time_reg(8, 34, 56), date_reg(25, 2, 30)),
            None
        );
    }

    #[test]
    fn rejects_years_outside_jh7110_range() {
        assert_eq!(
            decode_rtc_datetime(time_reg(8, 34, 56), date_reg(0, 6, 12)),
            None
        );
        assert_eq!(
            decode_rtc_datetime(time_reg(8, 34, 56), date_reg(100, 6, 12)),
            None
        );
    }
}
