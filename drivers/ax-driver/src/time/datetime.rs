pub(super) fn datetime_to_unix_timestamp(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<u64> {
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)?).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }
    if year < 1970 {
        return None;
    }

    let days_before_year = (1970..year).map(days_in_year).sum::<u64>();
    let days_before_month = (1..month)
        .map(|month| days_in_month(year, month).unwrap_or(0))
        .sum::<u32>() as u64;
    let days = days_before_year + days_before_month + u64::from(day - 1);
    Some(days * 86_400 + u64::from(hour) * 3_600 + u64::from(minute) * 60 + u64::from(second))
}

fn days_in_year(year: i32) -> u64 {
    if is_leap_year(year) { 366 } else { 365 }
}

fn days_in_month(year: i32, month: u32) -> Option<u32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
