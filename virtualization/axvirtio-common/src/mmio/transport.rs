/// MMIO transport layer utilities
use axaddrspace::{GuestPhysAddr, device::AccessWidth};

use crate::{VIRTIO_MMIO_CONFIG_OFFSET, VirtioResult, error::VirtioError};

/// Macro to handle bytes-to-value conversion for different access widths
macro_rules! convert_bytes_to_value {
    ($data:expr, $size:literal, $type:ty) => {{
        let (bytes, _) = $data
            .split_first_chunk::<$size>()
            .ok_or(VirtioError::InvalidBufferSize)?;
        Ok(<$type>::from_le_bytes(*bytes) as usize)
    }};
}

/// Macro to handle value-to-bytes conversion for different access widths
macro_rules! convert_value_to_bytes {
    ($data:expr, $val:expr, $size:literal, $type:ty) => {{
        $data[..$size].copy_from_slice(&($val as $type).to_le_bytes());
    }};
}

/// Validate MMIO access width
pub fn validate_access_width(width: AccessWidth) -> VirtioResult<()> {
    // VirtIO MMIO requires 32-bit accesses for registers
    if width != AccessWidth::Dword {
        return Err(VirtioError::InvalidAccessWidth);
    }
    Ok(())
}

/// Calculate register offset from base address
pub fn calculate_offset(addr: GuestPhysAddr, base_addr: GuestPhysAddr) -> usize {
    addr.as_usize() - base_addr.as_usize()
}

/// Check if address is within device range
pub fn is_address_in_range(addr: GuestPhysAddr, base_addr: GuestPhysAddr, size: usize) -> bool {
    let offset = addr.as_usize().saturating_sub(base_addr.as_usize());
    offset < size
}

/// Validate MMIO read access
pub fn validate_read_access(
    addr: GuestPhysAddr,
    width: AccessWidth,
    base_addr: GuestPhysAddr,
    size: usize,
) -> VirtioResult<usize> {
    // Check if address is in range
    if !is_address_in_range(addr, base_addr, size) {
        return Ok(0); // Return 0 for out-of-range reads
    }

    // Validate access width for configuration registers
    let offset = calculate_offset(addr, base_addr);
    if offset < VIRTIO_MMIO_CONFIG_OFFSET {
        // Configuration registers require 32-bit access
        validate_access_width(width)?;
    }

    Ok(offset)
}

/// Validate MMIO write access
pub fn validate_write_access(
    addr: GuestPhysAddr,
    width: AccessWidth,
    base_addr: GuestPhysAddr,
    size: usize,
) -> VirtioResult<usize> {
    // Check if address is in range
    if !is_address_in_range(addr, base_addr, size) {
        return Ok(0); // Ignore out-of-range writes
    }

    // Validate access width for configuration registers
    let offset = calculate_offset(addr, base_addr);
    if offset < VIRTIO_MMIO_CONFIG_OFFSET {
        // Configuration registers require 32-bit access
        validate_access_width(width)?;
    }

    Ok(offset)
}

/// Convert value to bytes based on width
pub fn value_to_bytes(val: usize, width: AccessWidth) -> [u8; 8] {
    let mut data = [0u8; 8];
    match width {
        AccessWidth::Byte => convert_value_to_bytes!(data, val, 1, u8),
        AccessWidth::Word => convert_value_to_bytes!(data, val, 2, u16),
        AccessWidth::Dword => convert_value_to_bytes!(data, val, 4, u32),
        AccessWidth::Qword => convert_value_to_bytes!(data, val, 8, u64),
    }
    data
}

/// Convert bytes to value based on width
pub fn bytes_to_value(data: &[u8], width: AccessWidth) -> VirtioResult<usize> {
    match width {
        AccessWidth::Byte => convert_bytes_to_value!(data, 1, u8),
        AccessWidth::Word => convert_bytes_to_value!(data, 2, u16),
        AccessWidth::Dword => convert_bytes_to_value!(data, 4, u32),
        AccessWidth::Qword => convert_bytes_to_value!(data, 8, u64),
    }
}
