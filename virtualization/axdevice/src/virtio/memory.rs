//! Checked guest-memory reads and writes for virtqueue structures.

use alloc::format;

use axvm_types::GuestPhysAddr;

use crate::{DeviceManagerError, DeviceManagerResult};

pub(crate) type GuestRead<'a> = &'a dyn Fn(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult;
pub(crate) type GuestWrite<'a> = &'a dyn Fn(GuestPhysAddr, &[u8]) -> DeviceManagerResult;

pub(crate) fn read_u16(
    read: GuestRead<'_>,
    base: u64,
    offset: u64,
    operation: &'static str,
) -> DeviceManagerResult<u16> {
    let mut bytes = [0; 2];
    read_guest_at(read, base, offset, &mut bytes, operation)?;
    Ok(u16::from_le_bytes(bytes))
}

pub(crate) fn read_u32(
    read: GuestRead<'_>,
    base: u64,
    offset: u64,
    operation: &'static str,
) -> DeviceManagerResult<u32> {
    let mut bytes = [0; 4];
    read_guest_at(read, base, offset, &mut bytes, operation)?;
    Ok(u32::from_le_bytes(bytes))
}

pub(crate) fn read_u64(
    read: GuestRead<'_>,
    base: u64,
    offset: u64,
    operation: &'static str,
) -> DeviceManagerResult<u64> {
    let mut bytes = [0; 8];
    read_guest_at(read, base, offset, &mut bytes, operation)?;
    Ok(u64::from_le_bytes(bytes))
}

pub(crate) fn write_u16(
    write: GuestWrite<'_>,
    base: u64,
    offset: u64,
    value: u16,
    operation: &'static str,
) -> DeviceManagerResult {
    write_guest_at(write, base, offset, &value.to_le_bytes(), operation)
}

pub(crate) fn write_u32(
    write: GuestWrite<'_>,
    base: u64,
    offset: u64,
    value: u32,
    operation: &'static str,
) -> DeviceManagerResult {
    write_guest_at(write, base, offset, &value.to_le_bytes(), operation)
}

pub(crate) fn read_guest(
    read: GuestRead<'_>,
    address: u64,
    buffer: &mut [u8],
    operation: &'static str,
) -> DeviceManagerResult {
    let address = checked_guest_address(address, buffer.len(), operation)?;
    read(address, buffer)
}

pub(crate) fn write_guest(
    write: GuestWrite<'_>,
    address: u64,
    buffer: &[u8],
    operation: &'static str,
) -> DeviceManagerResult {
    let address = checked_guest_address(address, buffer.len(), operation)?;
    write(address, buffer)
}

pub(crate) fn checked_guest_address(
    address: u64,
    length: usize,
    operation: &'static str,
) -> DeviceManagerResult<GuestPhysAddr> {
    let length = u64::try_from(length).map_err(|_| DeviceManagerError::InvalidInput {
        operation,
        detail: "guest buffer length does not fit 64 bits".into(),
    })?;
    let end = address
        .checked_add(length)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation,
            detail: format!("guest range {address:#x}+{length:#x} overflows"),
        })?;
    let address = usize::try_from(address).map_err(|_| DeviceManagerError::InvalidInput {
        operation,
        detail: "guest address does not fit the host address width".into(),
    })?;
    usize::try_from(end).map_err(|_| DeviceManagerError::InvalidInput {
        operation,
        detail: "guest range end does not fit the host address width".into(),
    })?;
    Ok(GuestPhysAddr::from_usize(address))
}

fn read_guest_at(
    read: GuestRead<'_>,
    base: u64,
    offset: u64,
    buffer: &mut [u8],
    operation: &'static str,
) -> DeviceManagerResult {
    let address = checked_offset(base, offset, operation)?;
    read_guest(read, address, buffer, operation)
}

fn write_guest_at(
    write: GuestWrite<'_>,
    base: u64,
    offset: u64,
    buffer: &[u8],
    operation: &'static str,
) -> DeviceManagerResult {
    let address = checked_offset(base, offset, operation)?;
    write_guest(write, address, buffer, operation)
}

fn checked_offset(base: u64, offset: u64, operation: &'static str) -> DeviceManagerResult<u64> {
    base.checked_add(offset)
        .ok_or_else(|| DeviceManagerError::InvalidInput {
            operation,
            detail: format!("guest address {base:#x}+{offset:#x} overflows"),
        })
}
