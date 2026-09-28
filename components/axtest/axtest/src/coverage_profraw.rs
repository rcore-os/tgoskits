use alloc::vec::Vec;
use core::{
    fmt,
    mem::{align_of, size_of},
    sync::atomic::{AtomicU64, Ordering},
};

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
/// The uniform counter pointer distinguishes this layout from older LLVM
/// profile records.
#[repr(C)]
struct LlvmProfileData {
    name_ref: u64,
    function_hash: u64,
    counter_ptr: usize,
    uniform_counter_ptr: usize,
    bitmap_ptr: usize,
    function_pointer: *const (),
    values: *mut (),
    num_counters: u32,
    num_value_sites: [u16; 3],
    offload_device_wave_size: u16,
    num_bitmap_bytes: u32,
}

const PROFILE_DATA_SIZE: usize = size_of::<LlvmProfileData>();
const COUNTER_PTR_OFFSET: usize = 16;
const UNIFORM_COUNTER_PTR_OFFSET: usize = 24;
const BITMAP_PTR_OFFSET: usize = 32;
const FUNCTION_PTR_OFFSET: usize = 40;
const VALUES_PTR_OFFSET: usize = 48;
const NUM_COUNTERS_OFFSET: usize = 56;
const NUM_VALUE_SITES_OFFSET: usize = 60;
const OFFLOAD_DEVICE_WAVE_SIZE_OFFSET: usize = 66;
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
    #[cfg(any(axtest_coverage, test))]
    InvalidSectionRange,
    InvalidDataSize,
    InvalidCounterSize,
    InvalidCounterAlignment,
    ArithmeticOverflow,
    CounterCountMismatch,
    BitmapCountMismatch,
    LiveBitmapUnsupported,
    ValueProfilingUnsupported,
    OffloadCoverageUnsupported,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(any(axtest_coverage, test))]
            Self::InvalidSectionRange => f.write_str("invalid LLVM profile section range"),
            Self::InvalidDataSize => f.write_str("invalid LLVM profile data record size"),
            Self::InvalidCounterSize => f.write_str("invalid LLVM profile counter size"),
            Self::InvalidCounterAlignment => f.write_str("unaligned LLVM profile counter section"),
            Self::ArithmeticOverflow => f.write_str("LLVM profile size overflow"),
            Self::CounterCountMismatch => {
                f.write_str("LLVM profile records do not match the counter section")
            }
            Self::BitmapCountMismatch => {
                f.write_str("LLVM profile records do not match the bitmap section")
            }
            Self::LiveBitmapUnsupported => {
                f.write_str("live LLVM profile bitmap capture is not supported")
            }
            Self::ValueProfilingUnsupported => {
                f.write_str("LLVM value profiling is not supported by axtest coverage")
            }
            Self::OffloadCoverageUnsupported => {
                f.write_str("LLVM offload coverage is not supported by axtest coverage")
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
    // SAFETY: the linker bounds cover live, immutable data and names sections
    // for the image lifetime. Writable counters and bitmap are passed only as
    // raw pointers; capture_sections checks them before accessing either.
    unsafe {
        capture_sections(
            section_slice(
                &raw const __start___llvm_prf_data,
                &raw const __stop___llvm_prf_data,
            )?,
            &raw const __start___llvm_prf_cnts,
            &raw const __stop___llvm_prf_cnts,
            &raw const __start___llvm_prf_bits,
            &raw const __stop___llvm_prf_bits,
            section_slice(
                &raw const __start___llvm_prf_names,
                &raw const __stop___llvm_prf_names,
            )?,
        )
    }
}

/// # Safety
/// The counter bounds must cover live, initialized `u64` counters whose
/// writers use only atomic updates for the duration of this call. Bitmap
/// bounds must refer to the same live image; nonempty bitmaps are rejected
/// without being read. `data` and `names` must not be mutated concurrently.
#[cfg(any(axtest_coverage, test))]
unsafe fn capture_sections(
    data: &[u8],
    counter_start: *const u8,
    counter_end: *const u8,
    bitmap_start: *const u8,
    bitmap_end: *const u8,
    names: &[u8],
) -> Result<Vec<u8>, ProfileError> {
    if section_len(bitmap_start, bitmap_end)? != 0 {
        return Err(ProfileError::LiveBitmapUnsupported);
    }
    // SAFETY: the caller provides live atomic counter storage, and the bounds
    // are passed unchanged to snapshot_counters for validation.
    let counters = unsafe { snapshot_counters(counter_start, counter_end)? };
    encode(ProfileSections {
        data,
        counters: &counters,
        bitmap: &[],
        names,
    })
}

/// # Safety
/// The bounds must cover live, initialized `u64` counters that are only
/// accessed atomically while this function executes.
#[cfg(any(axtest_coverage, test))]
unsafe fn snapshot_counters(start: *const u8, end: *const u8) -> Result<Vec<u8>, ProfileError> {
    let len = section_len(start, end)?;
    if len % size_of::<u64>() != 0 {
        return Err(ProfileError::InvalidCounterSize);
    }
    if !(start as usize).is_multiple_of(align_of::<AtomicU64>()) {
        return Err(ProfileError::InvalidCounterAlignment);
    }

    let mut snapshot = Vec::with_capacity(len);
    let counters = start.cast::<u64>().cast_mut();
    for index in 0..len / size_of::<u64>() {
        // SAFETY: the caller guarantees live, initialized atomic counters;
        // the range and alignment checks above cover each element. LLVM uses
        // atomic updates in coverage builds, so no non-atomic access races.
        let counter = unsafe { AtomicU64::from_ptr(counters.add(index)) };
        snapshot.extend_from_slice(&counter.load(Ordering::Relaxed).to_le_bytes());
    }
    Ok(snapshot)
}

#[cfg(axtest_coverage)]
/// # Safety
/// The bounds must refer to a live, immutable linker section for the image
/// lifetime, including a valid non-null pointer for an empty section.
unsafe fn section_slice(start: *const u8, end: *const u8) -> Result<&'static [u8], ProfileError> {
    let len = section_len(start, end)?;
    // SAFETY: callers pass linker-provided immutable section bounds, which
    // remain valid for the image lifetime and are checked above.
    Ok(unsafe { core::slice::from_raw_parts(start, len) })
}

#[cfg(any(axtest_coverage, test))]
fn section_len(start: *const u8, end: *const u8) -> Result<usize, ProfileError> {
    let len = (end as usize)
        .checked_sub(start as usize)
        .ok_or(ProfileError::InvalidSectionRange)?;
    if len > isize::MAX as usize {
        return Err(ProfileError::InvalidSectionRange);
    }
    Ok(len)
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
        0, // uniform counter count
        0, // padding after uniform counters
        0, // uniform counters delta
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
        if read_u64(record, UNIFORM_COUNTER_PTR_OFFSET) != 0
            || read_u16(record, OFFLOAD_DEVICE_WAVE_SIZE_OFFSET) != 0
        {
            return Err(ProfileError::OffloadCoverageUnsupported);
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
            UNIFORM_COUNTER_PTR_OFFSET,
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

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + size_of::<u64>()].try_into().unwrap())
}

fn patch_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    use super::*;
    extern crate std;

    use std::{path::Path, process::Command};

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
    fn live_capture_snapshots_counters_and_rejects_unsafe_sections() {
        let data = record(2);
        let counters = [AtomicU64::new(0), AtomicU64::new(0)];
        counters[0].store(7, Ordering::Relaxed);
        counters[1].store(13, Ordering::Relaxed);
        let start = counters.as_ptr().cast::<u8>();
        let end = unsafe { start.add(size_of_val(&counters)) };
        let empty_bitmap = [0u8; 0];
        let bitmap = empty_bitmap.as_ptr();

        // SAFETY: all pointers refer to live local atomic storage or an empty
        // bitmap; the invalid ranges below are rejected before being read.
        let capture = |data: &[u8],
                       start: *const u8,
                       end: *const u8,
                       bits: *const u8,
                       bits_end: *const u8,
                       names: &[u8]| unsafe {
            capture_sections(data, start, end, bits, bits_end, names)
        };
        let encoded = capture(&data, start, end, bitmap, bitmap, b"names").unwrap();
        let counter_offset = RAW_HEADER_SIZE + PROFILE_DATA_SIZE;
        assert_eq!(read_u64(&encoded, counter_offset), 7);
        assert_eq!(read_u64(&encoded, counter_offset + size_of::<u64>()), 13);

        let nonempty_bitmap = [AtomicU8::new(1)];
        let bits = nonempty_bitmap.as_ptr().cast::<u8>();
        let bits_end = unsafe { bits.add(1) };
        assert_eq!(
            capture(&data, start, end, bits, bits_end, b"names"),
            Err(ProfileError::LiveBitmapUnsupported)
        );

        let bytes = [0u8; 24];
        let unaligned = unsafe { bytes.as_ptr().add(1) };
        let unaligned_end = unsafe { unaligned.add(size_of::<u64>()) };
        assert_eq!(
            capture(&[], unaligned, unaligned_end, bitmap, bitmap, b""),
            Err(ProfileError::InvalidCounterAlignment)
        );
        let short_end = unsafe { start.add(size_of::<u64>() - 1) };
        assert_eq!(
            capture(&[], start, short_end, bitmap, bitmap, b""),
            Err(ProfileError::InvalidCounterSize)
        );
        assert_eq!(
            capture(&[], end, start, bitmap, bitmap, b""),
            Err(ProfileError::InvalidSectionRange)
        );
    }

    #[test]
    fn nonempty_bitmap_profile_merges_with_pinned_llvm() {
        let mut data = record(2);
        // NameRef is the first eight bytes of MD5("bitmap_fixture").
        data[..8].copy_from_slice(&0x67d2_06a4_6760_dcaeu64.to_le_bytes());
        data[8..16].copy_from_slice(&0x8267_d13e_6351_6cddu64.to_le_bytes());
        data[NUM_BITMAP_BYTES_OFFSET..NUM_BITMAP_BYTES_OFFSET + 4]
            .copy_from_slice(&1u32.to_le_bytes());
        let counters = [1u64.to_le_bytes(), 0u64.to_le_bytes()].concat();
        let encoded = encode(ProfileSections {
            data: &data,
            counters: &counters,
            bitmap: &[1],
            names: b"\x0e\x00bitmap_fixture",
        })
        .unwrap();

        let bitmap_delta = read_u64(&encoded, RAW_HEADER_SIZE + 32);
        assert_eq!(
            bitmap_delta,
            (PROFILE_DATA_SIZE + 2 * size_of::<u64>()) as u64
        );

        let sysroot = Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()
            .unwrap();
        assert!(sysroot.status.success());
        let rustlib =
            Path::new(std::str::from_utf8(&sysroot.stdout).unwrap().trim()).join("lib/rustlib");
        let profdata = std::fs::read_dir(rustlib)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("bin/llvm-profdata"))
            .find(|path| path.is_file())
            .expect("pinned toolchain must provide llvm-profdata");
        let raw_path =
            std::env::temp_dir().join(format!("axtest-bitmap-{}.profraw", std::process::id()));
        let merged_path =
            std::env::temp_dir().join(format!("axtest-bitmap-{}.profdata", std::process::id()));
        std::fs::write(&raw_path, &encoded).unwrap();
        let merged = Command::new(&profdata)
            .args(["merge", "-sparse", "--text"])
            .arg(&raw_path)
            .arg("-o")
            .arg(&merged_path)
            .output()
            .unwrap();
        std::fs::remove_file(&raw_path).unwrap();
        assert!(
            merged.status.success(),
            "{}",
            std::str::from_utf8(&merged.stderr).unwrap()
        );
        let merged_text = std::fs::read_to_string(&merged_path).unwrap();
        assert!(
            merged_text.contains("$1\n# Bitmap Byte Values:\n0x1\n"),
            "{merged_text}"
        );
        std::fs::remove_file(&merged_path).unwrap();
        assert!(merged_text.starts_with("bitmap_fixture\n"), "{merged_text}");
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
}
