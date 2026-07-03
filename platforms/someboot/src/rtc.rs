#[cfg(any(efi, test))]
const NANOS_PER_SEC: u64 = 1_000_000_000;
#[cfg(any(efi, test))]
const SECS_PER_DAY: i64 = 86_400;
#[cfg(any(efi, test))]
const UEFI_MIN_YEAR: i32 = 1900;
#[cfg(any(efi, test))]
const UEFI_MAX_YEAR: i32 = 9999;

#[cfg(any(efi, test))]
struct DateTimeParts {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    nanosecond: u32,
    timezone_minutes: Option<i16>,
}

/// Returns the current firmware wall-clock time as Unix epoch nanoseconds.
pub fn epoch_time_nanos() -> Option<u64> {
    epoch_time_nanos_impl()
}

#[cfg(efi)]
fn epoch_time_nanos_impl() -> Option<u64> {
    if !crate::efi_stub::is_uefi_available() {
        return None;
    }

    let time = uefi::runtime::get_time().ok()?;
    time.is_valid().ok()?;
    datetime_to_epoch_nanos(DateTimeParts {
        year: time.year(),
        month: time.month(),
        day: time.day(),
        hour: time.hour(),
        minute: time.minute(),
        second: time.second(),
        nanosecond: time.nanosecond(),
        timezone_minutes: time.time_zone(),
    })
}

#[cfg(not(efi))]
fn epoch_time_nanos_impl() -> Option<u64> {
    None
}

#[cfg(any(efi, test))]
fn datetime_to_epoch_nanos(datetime: DateTimeParts) -> Option<u64> {
    let year = i32::from(datetime.year);
    let month = u32::from(datetime.month);
    let day = u32::from(datetime.day);
    let hour = u32::from(datetime.hour);
    let minute = u32::from(datetime.minute);
    let second = u32::from(datetime.second);

    if !(UEFI_MIN_YEAR..=UEFI_MAX_YEAR).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)?).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
        || datetime.nanosecond >= NANOS_PER_SEC as u32
    {
        return None;
    }

    let days_since_epoch = days_from_civil(year, month, day)?;
    let seconds_of_day = i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    let timezone_offset = i64::from(datetime.timezone_minutes.unwrap_or(0)) * 60;
    let epoch_seconds = days_since_epoch
        .checked_mul(SECS_PER_DAY)?
        .checked_add(seconds_of_day)?
        .checked_sub(timezone_offset)?;

    if epoch_seconds < 0 {
        return None;
    }

    let epoch_seconds = u64::try_from(epoch_seconds).ok()?;
    epoch_seconds
        .checked_mul(NANOS_PER_SEC)?
        .checked_add(u64::from(datetime.nanosecond))
}

#[cfg(any(efi, test))]
fn days_in_month(year: i32, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

#[cfg(any(efi, test))]
fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(any(efi, test))]
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    let year = year - i32::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i32::try_from(month).ok()?;
    let day = i32::try_from(day).ok()?;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(i64::from(era) * 146_097 + i64::from(day_of_era) - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datetime_to_nanos(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
        nanosecond: u32,
        timezone_minutes: Option<i16>,
    ) -> Option<u64> {
        datetime_to_epoch_nanos(DateTimeParts {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanosecond,
            timezone_minutes,
        })
    }

    #[test]
    fn converts_utc_datetime_to_epoch_nanos() {
        assert_eq!(
            datetime_to_nanos(2024, 1, 2, 3, 4, 5, 6, None),
            Some(1_704_164_645_000_000_006)
        );
    }

    #[test]
    fn accepts_leap_day() {
        assert_eq!(
            datetime_to_nanos(2024, 2, 29, 0, 0, 0, 0, None),
            Some(1_709_164_800_000_000_000)
        );
    }

    #[test]
    fn applies_uefi_timezone_offset_minutes() {
        assert_eq!(
            datetime_to_nanos(2024, 1, 2, 3, 4, 5, 0, Some(480)),
            Some(1_704_135_845_000_000_000)
        );
    }

    #[test]
    fn treats_unspecified_timezone_as_utc() {
        assert_eq!(datetime_to_nanos(1970, 1, 1, 0, 0, 0, 0, None), Some(0));
    }

    #[test]
    fn rejects_invalid_datetime_fields() {
        assert_eq!(datetime_to_nanos(2023, 2, 29, 0, 0, 0, 0, None), None);
        assert_eq!(
            datetime_to_nanos(2024, 1, 1, 0, 0, 0, 1_000_000_000, None),
            None
        );
    }
}
