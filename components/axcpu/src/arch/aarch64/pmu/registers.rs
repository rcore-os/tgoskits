//! Named PMU register accesses, avoiding PMSELR races.
use core::arch::asm;
macro_rules! pmev_switch {
    // Read shape: yields the named register's value, 0 if out of range.
    (read $n:expr, $reg:literal) => {{
        macro_rules! arm {
                            ($idx:literal) => {{
                                let value: u64;
                                unsafe {
                                    asm!(concat!("mrs {}, ", $reg, $idx, "_EL0"), out(reg) value);
                                }
                                value
                            }};
                        }
        match $n {
            0 => arm!("0"),
            1 => arm!("1"),
            2 => arm!("2"),
            3 => arm!("3"),
            4 => arm!("4"),
            5 => arm!("5"),
            6 => arm!("6"),
            7 => arm!("7"),
            8 => arm!("8"),
            9 => arm!("9"),
            10 => arm!("10"),
            11 => arm!("11"),
            12 => arm!("12"),
            13 => arm!("13"),
            14 => arm!("14"),
            15 => arm!("15"),
            16 => arm!("16"),
            17 => arm!("17"),
            18 => arm!("18"),
            19 => arm!("19"),
            20 => arm!("20"),
            21 => arm!("21"),
            22 => arm!("22"),
            23 => arm!("23"),
            24 => arm!("24"),
            25 => arm!("25"),
            26 => arm!("26"),
            27 => arm!("27"),
            28 => arm!("28"),
            29 => arm!("29"),
            30 => arm!("30"),
            _ => unreachable!("validated PMU index"),
        }
    }};
    // Write shape: writes `$value` to the named register, no-op if out of range.
    (write $n:expr, $reg:literal, $value:expr) => {{
        let v: u64 = $value;
        macro_rules! arm {
                            ($idx:literal) => {{
                                unsafe {
                                    asm!(concat!("msr ", $reg, $idx, "_EL0, {}"), in(reg) v);
                                }
                            }};
                        }
        match $n {
            0 => arm!("0"),
            1 => arm!("1"),
            2 => arm!("2"),
            3 => arm!("3"),
            4 => arm!("4"),
            5 => arm!("5"),
            6 => arm!("6"),
            7 => arm!("7"),
            8 => arm!("8"),
            9 => arm!("9"),
            10 => arm!("10"),
            11 => arm!("11"),
            12 => arm!("12"),
            13 => arm!("13"),
            14 => arm!("14"),
            15 => arm!("15"),
            16 => arm!("16"),
            17 => arm!("17"),
            18 => arm!("18"),
            19 => arm!("19"),
            20 => arm!("20"),
            21 => arm!("21"),
            22 => arm!("22"),
            23 => arm!("23"),
            24 => arm!("24"),
            25 => arm!("25"),
            26 => arm!("26"),
            27 => arm!("27"),
            28 => arm!("28"),
            29 => arm!("29"),
            30 => arm!("30"),
            _ => unreachable!("validated PMU index"),
        }
    }};
}

pub(super) fn read_counter(index: usize) -> u64 {
    pmev_switch!(read index, "PMEVCNTR")
}
pub(super) fn write_counter(index: usize, value: u64) {
    pmev_switch!(write index, "PMEVCNTR", value);
}
pub(super) fn write_event(index: usize, value: u64) {
    pmev_switch!(write index, "PMEVTYPER", value);
}
