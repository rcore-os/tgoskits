pub(super) const REG_SECONDS: u8 = 0x00;
pub(super) const REG_MINUTES: u8 = 0x02;
pub(super) const REG_HOURS: u8 = 0x04;
pub(super) const REG_DAY_OF_MONTH: u8 = 0x07;
pub(super) const REG_MONTH: u8 = 0x08;
pub(super) const REG_YEAR: u8 = 0x09;
pub(super) const REG_B: u8 = 0x0b;
pub(super) const REG_CENTURY: u8 = 0x32;
pub(super) const REG_B_24H: u8 = 1 << 1;
pub(super) const REG_B_BINARY: u8 = 1 << 2;

use super::datetime::datetime_to_unix_timestamp;

pub(super) fn snapshot_to_unix_timestamp(snapshot: &[u8; 128]) -> Option<u64> {
    let reg_b = snapshot[REG_B as usize];
    let binary = reg_b & REG_B_BINARY != 0;
    let hour_reg = snapshot[REG_HOURS as usize];
    let pm = hour_reg & 0x80 != 0;

    let second = decode(snapshot[REG_SECONDS as usize], binary)?;
    let minute = decode(snapshot[REG_MINUTES as usize], binary)?;
    let mut hour = decode(hour_reg & 0x7f, binary)?;
    let day = decode(snapshot[REG_DAY_OF_MONTH as usize], binary)?;
    let month = decode(snapshot[REG_MONTH as usize], binary)?;
    let year_low = decode(snapshot[REG_YEAR as usize], binary)?;
    let century = decode(snapshot[REG_CENTURY as usize], binary)
        .filter(|century| *century != 0)
        .unwrap_or(20);

    if reg_b & REG_B_24H == 0 {
        if pm {
            hour = (hour % 12) + 12;
        } else if hour == 12 {
            hour = 0;
        }
    }

    let year = (century * 100 + year_low) as i32;
    datetime_to_unix_timestamp(year, month, day, hour, minute, second)
}

fn decode(value: u8, binary: bool) -> Option<u32> {
    if binary {
        Some(u32::from(value))
    } else {
        let lo = value & 0x0f;
        let hi = value >> 4;
        (lo < 10 && hi < 10).then_some(u32::from(hi) * 10 + u32::from(lo))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REG_B, REG_B_24H, REG_B_BINARY, REG_CENTURY, REG_DAY_OF_MONTH, REG_HOURS, REG_MINUTES,
        REG_MONTH, REG_SECONDS, REG_YEAR, snapshot_to_unix_timestamp,
    };

    #[test]
    fn decodes_bcd_24h_with_century() {
        let mut regs = [0u8; 128];
        regs[REG_SECONDS as usize] = 0x56;
        regs[REG_MINUTES as usize] = 0x34;
        regs[REG_HOURS as usize] = 0x08;
        regs[REG_DAY_OF_MONTH as usize] = 0x12;
        regs[REG_MONTH as usize] = 0x06;
        regs[REG_YEAR as usize] = 0x25;
        regs[REG_CENTURY as usize] = 0x20;
        regs[REG_B as usize] = REG_B_24H;

        let timestamp = snapshot_to_unix_timestamp(&regs).unwrap();

        assert_eq!(timestamp, 1_749_717_296);
    }

    #[test]
    fn decodes_binary_12h_pm_without_century() {
        let mut regs = [0u8; 128];
        regs[REG_SECONDS as usize] = 56;
        regs[REG_MINUTES as usize] = 34;
        regs[REG_HOURS as usize] = 0x80 | 8;
        regs[REG_DAY_OF_MONTH as usize] = 12;
        regs[REG_MONTH as usize] = 6;
        regs[REG_YEAR as usize] = 25;
        regs[REG_B as usize] = REG_B_BINARY;

        let timestamp = snapshot_to_unix_timestamp(&regs).unwrap();

        assert_eq!(timestamp, 1_749_760_496);
    }
}
