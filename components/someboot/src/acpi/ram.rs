//! ACPI 内存信息获取模块
//!
//! 通过解析 ACPI AML 获取系统物理内存地址和大小

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use aml::{AmlContext, AmlName, DebugVerbosity};
use bit_field::BitField;
use byteorder::{ByteOrder, LittleEndian};
use core::fmt;
use log::{debug, error, info, warn};

use crate::mem::phys_to_virt;

extern crate alloc;

#[derive(Debug, Clone)]
pub enum RamError {
    AcpiNotAvailable,
    AmlParseError(aml::AmlError),
    NoMemoryRegions,
}

impl fmt::Display for RamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RamError::AcpiNotAvailable => write!(f, "ACPI tables not available"),
            RamError::AmlParseError(e) => write!(f, "AML parse error: {:?}", e),
            RamError::NoMemoryRegions => write!(f, "No memory regions found"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
}

impl fmt::Display for MemoryRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MemoryRegion: base={:#x}, length={:#x} ({} MB)",
            self.base,
            self.length,
            self.length / (1024 * 1024)
        )
    }
}

struct AmlHandler;

impl aml::Handler for AmlHandler {
    fn read_u8(&self, address: usize) -> u8 {
        unsafe { *(phys_to_virt(address) as *const u8) }
    }

    fn read_u16(&self, address: usize) -> u16 {
        unsafe { *(phys_to_virt(address) as *const u16) }
    }

    fn read_u32(&self, address: usize) -> u32 {
        unsafe { *(phys_to_virt(address) as *const u32) }
    }

    fn read_u64(&self, address: usize) -> u64 {
        unsafe { *(phys_to_virt(address) as *const u64) }
    }

    fn write_u8(&mut self, address: usize, value: u8) {
        unsafe {
            *(phys_to_virt(address) as *mut u8) = value;
        }
    }

    fn write_u16(&mut self, address: usize, value: u16) {
        unsafe {
            *(phys_to_virt(address) as *mut u16) = value;
        }
    }

    fn write_u32(&mut self, address: usize, value: u32) {
        unsafe {
            *(phys_to_virt(address) as *mut u32) = value;
        }
    }

    fn write_u64(&mut self, address: usize, value: u64) {
        unsafe {
            *(phys_to_virt(address) as *mut u64) = value;
        }
    }

    fn read_io_u8(&self, _port: u16) -> u8 {
        0
    }

    fn read_io_u16(&self, _port: u16) -> u16 {
        0
    }

    fn read_io_u32(&self, _port: u16) -> u32 {
        0
    }

    fn write_io_u8(&self, _port: u16, _value: u8) {}

    fn write_io_u16(&self, _port: u16, _value: u16) {}

    fn write_io_u32(&self, _port: u16, _value: u32) {}

    fn read_pci_u8(&self, _segment: u16, _bus: u8, _device: u8, _function: u8, _offset: u16) -> u8 {
        0
    }

    fn read_pci_u16(
        &self,
        _segment: u16,
        _bus: u8,
        _device: u8,
        _function: u8,
        _offset: u16,
    ) -> u16 {
        0
    }

    fn read_pci_u32(
        &self,
        _segment: u16,
        _bus: u8,
        _device: u8,
        _function: u8,
        _offset: u16,
    ) -> u32 {
        0
    }

    fn write_pci_u8(
        &self,
        _segment: u16,
        _bus: u8,
        _device: u8,
        _function: u8,
        _offset: u16,
        _value: u8,
    ) {
    }

    fn write_pci_u16(
        &self,
        _segment: u16,
        _bus: u8,
        _device: u8,
        _function: u8,
        _offset: u16,
        _value: u16,
    ) {
    }

    fn write_pci_u32(
        &self,
        _segment: u16,
        _bus: u8,
        _device: u8,
        _function: u8,
        _offset: u16,
        _value: u32,
    ) {
    }
}

pub fn get_memory_regions() -> Result<Vec<MemoryRegion>, RamError> {
    let tables = crate::acpi::tables().map_err(|_| RamError::AcpiNotAvailable)?;

    let dsdt = tables.dsdt().map_err(|e| {
        error!("Failed to get DSDT: {:?}", e);
        RamError::AcpiNotAvailable
    })?;

    let dsdt_ptr = phys_to_virt(dsdt.phys_address) as *const u8;
    let dsdt_len = dsdt.length as usize;

    debug!("DSDT at {:#x}, length: {}", dsdt.phys_address, dsdt_len);

    let mut context = AmlContext::new(Box::new(AmlHandler), DebugVerbosity::None);

    let dsdt_slice = unsafe { core::slice::from_raw_parts(dsdt_ptr, dsdt_len) };

    let aml_start = 36;
    if dsdt_len <= aml_start {
        return Err(RamError::NoMemoryRegions);
    }

    context
        .parse_table(&dsdt_slice[aml_start..])
        .map_err(RamError::AmlParseError)?;

    for ssdt in tables.ssdts() {
        let ssdt_ptr = phys_to_virt(ssdt.phys_address) as *const u8;
        let ssdt_len = ssdt.length as usize;
        if ssdt_len > 36 {
            let ssdt_slice = unsafe { core::slice::from_raw_parts(ssdt_ptr, ssdt_len) };
            if let Err(e) = context.parse_table(&ssdt_slice[36..]) {
                warn!("Failed to parse SSDT: {:?}", e);
            }
        }
    }

    let mut regions = Vec::new();

    let memory_devices = [
        "\\_SB_.MEM0",
        "\\_SB_.MEM_",
        "\\_SB_.MEM",
        "\\_SB_.MEM1",
        "\\_SB_.MEM2",
        "\\_SB_.MEM3",
        "\\_SB_.PCI0._CRS",
        "\\_SB_.PC00._CRS",
        "\\_SB_.PCI_._CRS",
    ];

    for device_path in &memory_devices {
        if let Ok(name) = AmlName::from_str(device_path) {
            if let Ok(value) = context.invoke_method(&name, aml::value::Args::EMPTY) {
                debug!("Found memory resource at {}", device_path);
                extract_memory_regions_from_buffer(&value, device_path, &mut regions);
            }
        }
    }

    scan_namespace_for_memory(&mut context, &mut regions);

    if regions.is_empty() {
        Err(RamError::NoMemoryRegions)
    } else {
        regions.sort_by_key(|r| r.base);
        regions.dedup_by(|a, b| a.base == b.base && a.length == b.length);
        Ok(regions)
    }
}

fn extract_memory_regions_from_buffer(
    value: &aml::AmlValue,
    source: &str,
    regions: &mut Vec<MemoryRegion>,
) {
    let buffer = match value {
        aml::AmlValue::Buffer(buf) => buf.lock(),
        _ => return,
    };

    let bytes = buffer.as_slice();
    let mut offset = 0;

    while offset < bytes.len() {
        if bytes[offset].get_bit(7) {
            let result = parse_large_descriptor(&bytes[offset..]);
            match result {
                Ok((Some(region), consumed)) => {
                    info!("Found memory region from {}: {}", source, region);
                    regions.push(region);
                    offset += consumed;
                }
                Ok((None, consumed)) => {
                    offset += consumed;
                }
                Err(_) => break,
            }
        } else {
            let consumed = parse_small_descriptor(&bytes[offset..]);
            if consumed == 0 {
                break;
            }
            offset += consumed;
        }
    }
}

fn parse_large_descriptor(bytes: &[u8]) -> Result<(Option<MemoryRegion>, usize), ()> {
    if bytes.len() < 3 {
        return Err(());
    }

    let descriptor_type = bytes[0].get_bits(0..7);
    let length = LittleEndian::read_u16(&bytes[1..=2]) as usize;
    let total_len = length + 3;

    if bytes.len() < total_len {
        return Err(());
    }

    match descriptor_type {
        0x07 => {
            let desc_bytes = &bytes[3..total_len];
            if desc_bytes.len() >= 15 {
                let resource_type = desc_bytes[0];
                if resource_type == 0 {
                    let min = LittleEndian::read_u32(&desc_bytes[8..12]);
                    let _max = LittleEndian::read_u32(&desc_bytes[12..16]);
                    let length = LittleEndian::read_u32(&desc_bytes[16..20]);
                    if length > 0 {
                        return Ok((
                            Some(MemoryRegion {
                                base: min as u64,
                                length: length as u64,
                            }),
                            total_len,
                        ));
                    }
                }
            }
            Ok((None, total_len))
        }
        0x08 => {
            let desc_bytes = &bytes[3..total_len];
            if desc_bytes.len() >= 13 {
                let resource_type = desc_bytes[0];
                if resource_type == 0 {
                    let min = LittleEndian::read_u16(&desc_bytes[6..8]);
                    let _max = LittleEndian::read_u16(&desc_bytes[8..10]);
                    let length = LittleEndian::read_u16(&desc_bytes[12..14]);
                    if length > 0 {
                        return Ok((
                            Some(MemoryRegion {
                                base: min as u64,
                                length: length as u64,
                            }),
                            total_len,
                        ));
                    }
                }
            }
            Ok((None, total_len))
        }
        0x0a => {
            let desc_bytes = &bytes[3..total_len];
            if desc_bytes.len() >= 27 {
                let resource_type = desc_bytes[0];
                if resource_type == 0 {
                    let min = LittleEndian::read_u64(&desc_bytes[12..20]);
                    let _max = LittleEndian::read_u64(&desc_bytes[20..28]);
                    let length = LittleEndian::read_u64(&desc_bytes[28..36]);
                    if length > 0 {
                        return Ok((Some(MemoryRegion { base: min, length }), total_len));
                    }
                }
            }
            Ok((None, total_len))
        }
        0x06 => {
            let desc_bytes = &bytes[3..total_len];
            if desc_bytes.len() >= 9 {
                let base = LittleEndian::read_u32(&desc_bytes[4..8]);
                let length = LittleEndian::read_u32(&desc_bytes[8..12]);
                if length > 0 {
                    return Ok((
                        Some(MemoryRegion {
                            base: base as u64,
                            length: length as u64,
                        }),
                        total_len,
                    ));
                }
            }
            Ok((None, total_len))
        }
        _ => Ok((None, total_len)),
    }
}

fn parse_small_descriptor(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }

    let length = bytes[0].get_bits(0..=2) as usize;
    let descriptor_type = bytes[0].get_bits(3..=6);

    if descriptor_type == 0x0f {
        return 0;
    }

    length + 1
}

fn scan_namespace_for_memory(context: &mut AmlContext, regions: &mut Vec<MemoryRegion>) {
    let _result = context.namespace.traverse(|name, level| {
        for (child_name, _handle) in level.values.iter() {
            if child_name.as_str() == "_CRS" {
                let crs_path = name.child(*child_name);
                if let Ok(value) = context.invoke_method(&crs_path, aml::value::Args::EMPTY) {
                    let source = crs_path.as_string();
                    extract_memory_regions_from_buffer(&value, &source, regions);
                }
            }
        }
        Ok(true)
    });
}
