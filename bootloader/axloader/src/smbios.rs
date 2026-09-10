extern crate alloc;

use alloc::string::{String, ToString};
use core::str;

use httpboot_protocol::LoaderHardwareInfo;

/// Parses SMBIOS structures and returns owned System Information (Type 1)
/// strings. The input is never retained by the result.
pub fn parse_type1(table: &[u8]) -> Option<LoaderHardwareInfo> {
    let mut offset = 0usize;
    while offset.checked_add(4)? <= table.len() {
        let structure_type = table[offset];
        let formatted_length = table[offset + 1] as usize;
        if formatted_length < 4 || offset.checked_add(formatted_length)? > table.len() {
            return None;
        }
        let strings_start = offset + formatted_length;
        let structure_end = find_string_set_end(table, strings_start)?;
        if structure_type == 1 {
            if formatted_length < 8 {
                return None;
            }
            let strings = &table[strings_start..structure_end - 2];
            return Some(LoaderHardwareInfo {
                manufacturer: smbios_string(strings, table[offset + 4]),
                product: smbios_string(strings, table[offset + 5]),
                version: smbios_string(strings, table[offset + 6]),
                serial: smbios_string(strings, table[offset + 7]),
            });
        }
        if structure_type == 127 {
            return None;
        }
        offset = structure_end;
    }
    None
}

fn find_string_set_end(table: &[u8], start: usize) -> Option<usize> {
    if start >= table.len() {
        return None;
    }
    table[start..]
        .windows(2)
        .position(|window| window == [0, 0])
        .and_then(|relative| start.checked_add(relative + 2))
}

fn smbios_string(strings: &[u8], index: u8) -> Option<String> {
    if index == 0 {
        return None;
    }
    let value = strings
        .split(|byte| *byte == 0)
        .nth(usize::from(index - 1))?;
    let value = str::from_utf8(value).ok()?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::parse_type1;

    #[test]
    fn parses_owned_type1_hardware_strings() {
        let table = type1_table(b"Acme\0Virtual Board\0v1\0SN42\0\0", [1, 2, 3, 4]);
        let info = parse_type1(&table).unwrap();
        assert_eq!(info.manufacturer.as_deref(), Some("Acme"));
        assert_eq!(info.product.as_deref(), Some("Virtual Board"));
        assert_eq!(info.version.as_deref(), Some("v1"));
        assert_eq!(info.serial.as_deref(), Some("SN42"));
    }

    #[test]
    fn rejects_short_and_unterminated_structures() {
        assert!(parse_type1(&[1, 3, 0, 0, 0, 0]).is_none());
        assert!(parse_type1(&type1_table(b"Acme\0", [1, 0, 0, 0])).is_none());
    }

    #[test]
    fn malformed_string_indexes_and_utf8_do_not_escape_the_parser() {
        let table = type1_table(&[0xff, 0, b'X', 0, 0], [1, 9, 0, 0]);
        let info = parse_type1(&table).unwrap();
        assert_eq!(info.manufacturer, None);
        assert_eq!(info.product, None);
        assert_eq!(info.version, None);
        assert_eq!(info.serial, None);
    }

    fn type1_table(strings: &[u8], indexes: [u8; 4]) -> Vec<u8> {
        let mut table = vec![1, 8, 0, 0];
        table.extend_from_slice(&indexes);
        table.extend_from_slice(strings);
        table
    }
}
