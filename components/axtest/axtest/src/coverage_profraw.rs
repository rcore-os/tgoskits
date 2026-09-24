use alloc::vec::Vec;
use core::{fmt, mem::size_of};

const RAW_PROFILE_MAGIC_64: u64 = 0xff6c_7072_6f66_7281;
const RAW_PROFILE_VERSION: u64 = 11;
const RAW_HEADER_FIELDS: usize = 19;
const RAW_HEADER_SIZE: usize = RAW_HEADER_FIELDS * size_of::<u64>();
const VALUE_KIND_LAST: u64 = 2;

// LLVM emits a reference to this symbol to force inclusion of its profiling
// runtime. Axtest supplies that runtime itself because builds use
// `-Zno-profiler-runtime`.
#[cfg(axtest_coverage)]
#[unsafe(no_mangle)]
static __llvm_profile_runtime: u8 = 0;

#[cfg(axtest_coverage)]
#[unsafe(no_mangle)]
static __llvm_profile_raw_version: u64 = RAW_PROFILE_VERSION;

#[cfg(not(target_pointer_width = "64"))]
compile_error!("axtest coverage currently requires a 64-bit target");
#[cfg(not(target_endian = "little"))]
compile_error!("axtest coverage currently requires a little-endian target");

/// LLVM 23's per-function coverage record.
///
/// Keep this layout in sync with the repository's pinned Rust toolchain. The
/// MC/DC pointer and record count distinguish raw profile version 11 from the
/// version 10 layout used by older LLVM releases.
#[repr(C)]
struct LlvmProfileData {
    name_ref: u64,
    function_hash: u64,
    counter_ptr: usize,
    bitmap_ptr: usize,
    mcdc_bitmap_ptr: usize,
    function_pointer: *const (),
    values: *mut (),
    num_counters: u32,
    num_value_sites: [u16; 3],
    num_mcdc_records: u16,
    num_bitmap_bytes: u32,
}

const PROFILE_DATA_SIZE: usize = size_of::<LlvmProfileData>();
const COUNTER_PTR_OFFSET: usize = 16;
const BITMAP_PTR_OFFSET: usize = 24;
const MCDC_BITMAP_PTR_OFFSET: usize = 32;
const FUNCTION_PTR_OFFSET: usize = 40;
const VALUES_PTR_OFFSET: usize = 48;
const NUM_COUNTERS_OFFSET: usize = 56;
const NUM_VALUE_SITES_OFFSET: usize = 60;
const NUM_MCDC_RECORDS_OFFSET: usize = 66;
const NUM_BITMAP_BYTES_OFFSET: usize = 68;

#[derive(Clone, Copy)]
struct ProfileSections<'a> {
    data: &'a [u8],
    counters: &'a [u8],
    bitmap: &'a [u8],
    names: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProfileError {
    #[cfg(axtest_coverage)]
    InvalidSectionRange,
    InvalidDataSize,
    InvalidCounterSize,
    ArithmeticOverflow,
    CounterCountMismatch,
    BitmapCountMismatch,
    ValueProfilingUnsupported,
    McdcUnsupported,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(axtest_coverage)]
            Self::InvalidSectionRange => f.write_str("invalid LLVM profile section range"),
            Self::InvalidDataSize => f.write_str("invalid LLVM profile data record size"),
            Self::InvalidCounterSize => f.write_str("invalid LLVM profile counter size"),
            Self::ArithmeticOverflow => f.write_str("LLVM profile size overflow"),
            Self::CounterCountMismatch => {
                f.write_str("LLVM profile records do not match the counter section")
            }
            Self::BitmapCountMismatch => {
                f.write_str("LLVM profile records do not match the bitmap section")
            }
            Self::ValueProfilingUnsupported => {
                f.write_str("LLVM value profiling is not supported by axtest coverage")
            }
            Self::McdcUnsupported => {
                f.write_str("LLVM MC/DC coverage is not supported by axtest coverage")
            }
        }
    }
}

#[cfg(axtest_coverage)]
unsafe extern "C" {
    static __start___llvm_prf_data: u8;
    static __stop___llvm_prf_data: u8;
    static __start___llvm_prf_cnts: u8;
    static __stop___llvm_prf_cnts: u8;
    static __start___llvm_prf_bits: u8;
    static __stop___llvm_prf_bits: u8;
    static __start___llvm_prf_names: u8;
    static __stop___llvm_prf_names: u8;
}

#[cfg(axtest_coverage)]
pub(super) fn capture() -> Result<Vec<u8>, ProfileError> {
    // SAFETY: the linker defines each pair as immutable bounds of a live
    // coverage section. Tests have completed before capture starts, so no
    // other execution context may mutate the counters while they are copied.
    let sections = unsafe {
        ProfileSections {
            data: section_slice(
                &raw const __start___llvm_prf_data,
                &raw const __stop___llvm_prf_data,
            )?,
            counters: section_slice(
                &raw const __start___llvm_prf_cnts,
                &raw const __stop___llvm_prf_cnts,
            )?,
            bitmap: section_slice(
                &raw const __start___llvm_prf_bits,
                &raw const __stop___llvm_prf_bits,
            )?,
            names: section_slice(
                &raw const __start___llvm_prf_names,
                &raw const __stop___llvm_prf_names,
            )?,
        }
    };
    encode(sections)
}

#[cfg(axtest_coverage)]
unsafe fn section_slice(start: *const u8, end: *const u8) -> Result<&'static [u8], ProfileError> {
    let len = (end as usize)
        .checked_sub(start as usize)
        .ok_or(ProfileError::InvalidSectionRange)?;
    // SAFETY: callers pass linker-provided section bounds. Subtraction above
    // proves their order, and both symbols remain valid for the image lifetime.
    Ok(unsafe { core::slice::from_raw_parts(start, len) })
}

fn encode(sections: ProfileSections<'_>) -> Result<Vec<u8>, ProfileError> {
    if PROFILE_DATA_SIZE != 72 || !sections.data.len().is_multiple_of(PROFILE_DATA_SIZE) {
        return Err(ProfileError::InvalidDataSize);
    }
    if !sections.counters.len().is_multiple_of(size_of::<u64>()) {
        return Err(ProfileError::InvalidCounterSize);
    }

    let counters_padding = padding(sections.counters.len());
    let bitmap_padding = padding(sections.bitmap.len());
    let names_padding = padding(sections.names.len());
    let data_start = RAW_HEADER_SIZE;
    let counters_start = checked_add(data_start, sections.data.len())?;
    let bitmap_start = checked_add(
        checked_add(counters_start, sections.counters.len())?,
        counters_padding,
    )?;
    let names_start = checked_add(
        checked_add(bitmap_start, sections.bitmap.len())?,
        bitmap_padding,
    )?;
    let encoded_size = checked_add(
        checked_add(names_start, sections.names.len())?,
        names_padding,
    )?;

    let records = sections.data.len() / PROFILE_DATA_SIZE;
    let mut output = Vec::with_capacity(encoded_size);
    for field in [
        RAW_PROFILE_MAGIC_64,
        RAW_PROFILE_VERSION,
        0, // binary IDs size
        usize_to_u64(records)?,
        0, // padding before counters
        usize_to_u64(sections.counters.len() / size_of::<u64>())?,
        usize_to_u64(counters_padding)?,
        usize_to_u64(sections.bitmap.len())?,
        usize_to_u64(bitmap_padding)?,
        0, // raw v11 MC/DC header field
        0, // raw v11 MC/DC header field
        0, // raw v11 MC/DC header field
        usize_to_u64(sections.names.len())?,
        usize_to_u64(counters_start - data_start)?,
        usize_to_u64(bitmap_start - data_start)?,
        usize_to_u64(names_start - data_start)?,
        0, // vtable records
        0, // vtable names size
        VALUE_KIND_LAST,
    ] {
        output.extend_from_slice(&field.to_le_bytes());
    }

    let mut counter_offset = 0usize;
    let mut bitmap_offset = 0usize;
    for record in sections.data.chunks_exact(PROFILE_DATA_SIZE) {
        let record_start = output.len();
        output.extend_from_slice(record);

        if read_u16(record, NUM_VALUE_SITES_OFFSET) != 0
            || read_u16(record, NUM_VALUE_SITES_OFFSET + 2) != 0
            || read_u16(record, NUM_VALUE_SITES_OFFSET + 4) != 0
        {
            return Err(ProfileError::ValueProfilingUnsupported);
        }
        if read_u16(record, NUM_MCDC_RECORDS_OFFSET) != 0 {
            return Err(ProfileError::McdcUnsupported);
        }

        let counter_address = checked_add(counters_start, counter_offset)?;
        let bitmap_address = checked_add(bitmap_start, bitmap_offset)?;
        patch_u64(
            &mut output,
            record_start + COUNTER_PTR_OFFSET,
            usize_to_u64(counter_address - record_start)?,
        );
        patch_u64(
            &mut output,
            record_start + BITMAP_PTR_OFFSET,
            usize_to_u64(bitmap_address - record_start)?,
        );
        for offset in [
            MCDC_BITMAP_PTR_OFFSET,
            FUNCTION_PTR_OFFSET,
            VALUES_PTR_OFFSET,
        ] {
            patch_u64(&mut output, record_start + offset, 0);
        }

        let counters = read_u32(record, NUM_COUNTERS_OFFSET) as usize;
        counter_offset = checked_add(
            counter_offset,
            counters
                .checked_mul(size_of::<u64>())
                .ok_or(ProfileError::ArithmeticOverflow)?,
        )?;
        bitmap_offset = checked_add(
            bitmap_offset,
            read_u32(record, NUM_BITMAP_BYTES_OFFSET) as usize,
        )?;
    }

    if counter_offset != sections.counters.len() {
        return Err(ProfileError::CounterCountMismatch);
    }
    if bitmap_offset != sections.bitmap.len() {
        return Err(ProfileError::BitmapCountMismatch);
    }

    output.extend_from_slice(sections.counters);
    output.resize(output.len() + counters_padding, 0);
    output.extend_from_slice(sections.bitmap);
    output.resize(output.len() + bitmap_padding, 0);
    output.extend_from_slice(sections.names);
    output.resize(output.len() + names_padding, 0);
    debug_assert_eq!(output.len(), encoded_size);
    Ok(output)
}

const fn padding(size: usize) -> usize {
    size.wrapping_neg() & 7
}

fn checked_add(left: usize, right: usize) -> Result<usize, ProfileError> {
    left.checked_add(right)
        .ok_or(ProfileError::ArithmeticOverflow)
}

fn usize_to_u64(value: usize) -> Result<u64, ProfileError> {
    value
        .try_into()
        .map_err(|_| ProfileError::ArithmeticOverflow)
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn patch_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(num_counters: u32) -> [u8; PROFILE_DATA_SIZE] {
        let mut record = [0; PROFILE_DATA_SIZE];
        record[NUM_COUNTERS_OFFSET..NUM_COUNTERS_OFFSET + 4]
            .copy_from_slice(&num_counters.to_le_bytes());
        record
    }

    #[test]
    fn llvm_23_profile_record_layout_is_72_bytes() {
        assert_eq!(PROFILE_DATA_SIZE, 72);
        assert_eq!(RAW_HEADER_SIZE, 152);
    }

    #[test]
    fn encodes_raw_v11_profile_with_file_relative_sections() {
        let mut data = Vec::new();
        data.extend_from_slice(&record(1));
        data.extend_from_slice(&record(2));
        let counters = [0u8; 3 * size_of::<u64>()];
        let encoded = encode(ProfileSections {
            data: &data,
            counters: &counters,
            bitmap: &[],
            names: b"names",
        })
        .unwrap();

        assert_eq!(read_u64(&encoded, 8), RAW_PROFILE_VERSION);
        assert_eq!(read_u64(&encoded, 3 * 8), 2);
        assert_eq!(read_u64(&encoded, 5 * 8), 3);
        assert_eq!(read_u64(&encoded, 12 * 8), 5);
        assert_eq!(
            read_u64(&encoded, RAW_HEADER_SIZE + COUNTER_PTR_OFFSET),
            (2 * PROFILE_DATA_SIZE) as u64
        );
        assert_eq!(
            read_u64(
                &encoded,
                RAW_HEADER_SIZE + PROFILE_DATA_SIZE + COUNTER_PTR_OFFSET,
            ),
            (PROFILE_DATA_SIZE + size_of::<u64>()) as u64
        );
    }

    #[test]
    fn rejects_value_profiling_before_export() {
        let mut data = record(0);
        data[NUM_VALUE_SITES_OFFSET..NUM_VALUE_SITES_OFFSET + 2]
            .copy_from_slice(&1u16.to_le_bytes());
        let err = encode(ProfileSections {
            data: &data,
            counters: &[],
            bitmap: &[],
            names: &[],
        })
        .unwrap_err();

        assert_eq!(err, ProfileError::ValueProfilingUnsupported);
    }

    fn read_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }
}
