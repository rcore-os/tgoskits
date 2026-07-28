// SPDX-License-Identifier: Apache-2.0 OR MIT

const ICH_SOURCE: &str = include_str!("../src/sys_reg/ich.rs");

fn field_layout(field: &str) -> (u32, u32) {
    let declaration = ICH_SOURCE
        .lines()
        .find(|line| line.trim_start().starts_with(field))
        .unwrap_or_else(|| panic!("missing {field} declaration"));
    let offset = declaration
        .split_once("OFFSET(")
        .and_then(|(_, tail)| tail.split_once(')'))
        .map(|(value, _)| value.parse().unwrap())
        .unwrap_or_else(|| panic!("missing {field} offset"));
    let width = declaration
        .split_once("NUMBITS(")
        .and_then(|(_, tail)| tail.split_once(')'))
        .map(|(value, _)| value.parse().unwrap())
        .unwrap_or_else(|| panic!("missing {field} width"));
    (offset, width)
}

fn field_mask(offset: u32, width: u32) -> u64 {
    ((1_u64 << width) - 1) << offset
}

#[test]
fn ich_vtr_idbits_covers_bits_25_through_23_without_overlapping_prebits() {
    let (idbits_offset, idbits_width) = field_layout("IDBITS");
    let (prebits_offset, prebits_width) = field_layout("PREBITS");
    let idbits_mask = field_mask(idbits_offset, idbits_width);
    let prebits_mask = field_mask(prebits_offset, prebits_width);

    assert_eq!(idbits_mask, 0x0380_0000);
    assert_eq!(prebits_mask, 0x1c00_0000);
    assert_eq!(idbits_mask & prebits_mask, 0);
}
