use core::slice;

use httpboot_protocol::LoaderHardwareInfo;
use uefi::table::cfg::ConfigTableEntry;

const MAX_SMBIOS_TABLE_SIZE: usize = 1024 * 1024;
const SMBIOS3_ENTRY_SIZE: usize = 24;
const SMBIOS2_ENTRY_SIZE: usize = 31;

pub fn hardware_info() -> LoaderHardwareInfo {
    uefi::system::with_config_table(|tables| {
        tables
            .iter()
            .find(|entry| entry.guid == ConfigTableEntry::SMBIOS3_GUID)
            .and_then(|entry| unsafe { table_from_smbios3_entry(entry.address.cast()) })
            .or_else(|| {
                tables
                    .iter()
                    .find(|entry| entry.guid == ConfigTableEntry::SMBIOS_GUID)
                    .and_then(|entry| unsafe { table_from_smbios2_entry(entry.address.cast()) })
            })
            .and_then(axloader::smbios::parse_type1)
            .unwrap_or_default()
    })
}

unsafe fn table_from_smbios3_entry<'a>(entry: *const u8) -> Option<&'a [u8]> {
    if entry.is_null() {
        return None;
    }
    // SAFETY: UEFI publishes SMBIOS3 entry points as readable configuration
    // tables. Only the fixed specification header is read before its address
    // and bounded table size are validated.
    let header = unsafe { slice::from_raw_parts(entry, SMBIOS3_ENTRY_SIZE) };
    if &header[..5] != b"_SM3_" || header[6] as usize != SMBIOS3_ENTRY_SIZE || checksum(header) != 0
    {
        return None;
    }
    let length = read_u32(header, 12)? as usize;
    let address = read_u64(header, 16)? as usize;
    unsafe { bounded_table(address, length) }
}

unsafe fn table_from_smbios2_entry<'a>(entry: *const u8) -> Option<&'a [u8]> {
    if entry.is_null() {
        return None;
    }
    // SAFETY: UEFI publishes SMBIOS2 entry points as readable configuration
    // tables. Only the fixed 31-byte entry point is read before validating the
    // checksum, table address and table length.
    let header = unsafe { slice::from_raw_parts(entry, SMBIOS2_ENTRY_SIZE) };
    if &header[..4] != b"_SM_"
        || header[5] as usize != SMBIOS2_ENTRY_SIZE
        || checksum(header) != 0
        || &header[16..21] != b"_DMI_"
        || checksum(&header[16..]) != 0
    {
        return None;
    }
    let length = read_u16(header, 22)? as usize;
    let address = read_u32(header, 24)? as usize;
    unsafe { bounded_table(address, length) }
}

unsafe fn bounded_table<'a>(address: usize, length: usize) -> Option<&'a [u8]> {
    if address == 0 || length == 0 || length > MAX_SMBIOS_TABLE_SIZE {
        return None;
    }
    // SAFETY: The address and length come from a checksummed firmware SMBIOS
    // entry point and are capped before constructing the temporary slice. The
    // slice never escapes the parsing call; all returned strings are owned.
    Some(unsafe { slice::from_raw_parts(address as *const u8, length) })
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}
