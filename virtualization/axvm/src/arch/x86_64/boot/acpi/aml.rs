//! x86 DSDT and SPCR construction.

use std::{string::ToString, vec::Vec};

use acpi_tables::{
    Aml,
    aml::{
        AddressSpace, AddressSpaceCacheable, Device, EISAName, IO, Interrupt, Memory32Fixed, Name,
        Package, PackageBuilder, ResourceTemplate, ZERO,
    },
    sdt::Sdt,
};

use super::{config::X86FirmwarePlan, serial::*};
use crate::boot::acpi::AcpiBuildError;

const OEM_ID: [u8; 6] = *b"AXVISR";
const OEM_TABLE_ID: [u8; 8] = *b"AXVMX86 ";
const OEM_REVISION: u32 = 1;

pub(super) fn build_dsdt(plan: &X86FirmwarePlan) -> Result<Vec<u8>, AcpiBuildError> {
    let mut aml = Vec::new();
    build_pci_device(plan, &mut aml);
    for serial in &plan.resources.serials {
        build_serial_device(serial, &mut aml)?;
    }
    build_fw_cfg_device(plan, &mut aml)?;

    let mut dsdt = Sdt::new(*b"DSDT", 36, 2, OEM_ID, OEM_TABLE_ID, OEM_REVISION);
    dsdt.append_slice(&aml);
    Ok(dsdt.as_slice().to_vec())
}

fn build_pci_device(plan: &X86FirmwarePlan, aml: &mut Vec<u8>) {
    let hid = Name::new("_HID".into(), &EISAName::new("PNP0A03"));
    let uid = Name::new("_UID".into(), &0u8);
    let adr = Name::new("_ADR".into(), &0u8);
    let seg = Name::new("_SEG".into(), &0u8);
    let bbn = Name::new("_BBN".into(), &0u8);
    let crs = Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(std::vec![
            &AddressSpace::new_bus_number(plan.pci.bus_range.0, plan.pci.bus_range.1),
            &AddressSpace::new_io(plan.pci.io_windows[0].0, plan.pci.io_windows[0].1, None,),
            &AddressSpace::new_io(plan.pci.io_windows[1].0, plan.pci.io_windows[1].1, None,),
            &AddressSpace::new_memory(
                cacheability(plan.pci.memory_windows[0].cacheable),
                true,
                plan.pci.memory_windows[0].start,
                plan.pci.memory_windows[0].end,
                None,
            ),
            &AddressSpace::new_memory(
                cacheability(plan.pci.memory_windows[1].cacheable),
                true,
                plan.pci.memory_windows[1].start,
                plan.pci.memory_windows[1].end,
                None,
            ),
        ]),
    );
    let mut routes = PackageBuilder::new();
    for device in 0u32..4 {
        for pin in 0u32..4 {
            let address = (device << 16) | 0xffff;
            let gsi = plan.pci.intx_gsis[((device + pin) & 3) as usize];
            routes.add_element(&Package::new(std::vec![&address, &pin, &ZERO, &gsi]));
        }
    }
    let prt = Name::new("_PRT".into(), &routes);
    Device::new(
        "_SB_.PCI0".into(),
        std::vec![&hid, &uid, &adr, &seg, &bbn, &crs, &prt],
    )
    .to_aml_bytes(aml);
}

const fn cacheability(cacheable: bool) -> AddressSpaceCacheable {
    if cacheable {
        AddressSpaceCacheable::Cacheable
    } else {
        AddressSpaceCacheable::NotCacheable
    }
}

fn build_serial_device(serial: &X86SerialPlan, aml: &mut Vec<u8>) -> Result<(), AcpiBuildError> {
    let hid_value = serial.hid.clone();
    let hid = Name::new("_HID".into(), &hid_value);
    let uid = Name::new("_UID".into(), &0u8);
    let interrupt = Interrupt::new(true, true, false, false, serial.irq);
    let path = serial
        .namespace_path
        .clone()
        .unwrap_or_else(|| std::format!("_SB_.{}", serial.name));
    match serial.registers {
        X86SerialRegisters::Port { base, size } => {
            let size = u8::try_from(size).map_err(|_| AcpiBuildError::InvalidValue {
                field: "serial PIO size",
                value: size.to_string(),
            })?;
            let registers = IO::new(base, base, 0, size);
            let resources = ResourceTemplate::new(std::vec![&interrupt, &registers]);
            let crs = Name::new("_CRS".into(), &resources);
            Device::new(path.as_str().into(), std::vec![&hid, &uid, &crs]).to_aml_bytes(aml);
        }
        X86SerialRegisters::Mmio { base, size } => {
            let registers = Memory32Fixed::new(true, base, size);
            let resources = ResourceTemplate::new(std::vec![&interrupt, &registers]);
            let crs = Name::new("_CRS".into(), &resources);
            Device::new(path.as_str().into(), std::vec![&hid, &uid, &crs]).to_aml_bytes(aml);
        }
    }
    Ok(())
}

fn build_fw_cfg_device(plan: &X86FirmwarePlan, aml: &mut Vec<u8>) -> Result<(), AcpiBuildError> {
    let selector_size = u8::try_from(plan.resources.fw_cfg_selector_size).map_err(|_| {
        AcpiBuildError::InvalidValue {
            field: "fw_cfg selector/data size",
            value: plan.resources.fw_cfg_selector_size.to_string(),
        }
    })?;
    let dma_size =
        u8::try_from(plan.resources.fw_cfg_dma_size).map_err(|_| AcpiBuildError::InvalidValue {
            field: "fw_cfg DMA size",
            value: plan.resources.fw_cfg_dma_size.to_string(),
        })?;
    let hid = Name::new("_HID".into(), &std::string::String::from("QEMU0002"));
    let sta = Name::new("_STA".into(), &0x0bu8);
    let crs = Name::new(
        "_CRS".into(),
        &ResourceTemplate::new(std::vec![
            &IO::new(
                plan.resources.fw_cfg_selector_base,
                plan.resources.fw_cfg_selector_base,
                0,
                selector_size,
            ),
            &IO::new(
                plan.resources.fw_cfg_dma_base,
                plan.resources.fw_cfg_dma_base,
                0,
                dma_size,
            ),
        ]),
    );
    Device::new("_SB_.FWCF".into(), std::vec![&hid, &sta, &crs]).to_aml_bytes(aml);
    Ok(())
}

pub(super) fn build_spcr(plan: &X86FirmwarePlan) -> Vec<u8> {
    const HEADER_SIZE: usize = 36;
    const INFO_SIZE: usize = 52;
    let serial = plan
        .resources
        .serials
        .first()
        .expect("the x86 firmware plan validates console0");
    let namespace = serial.namespace_path.as_deref().unwrap_or(".");
    let namespace_length = namespace.len() + 1;
    let mut spcr = Sdt::new(
        *b"SPCR",
        (HEADER_SIZE + INFO_SIZE + namespace_length) as u32,
        4,
        OEM_ID,
        OEM_TABLE_ID,
        OEM_REVISION,
    );
    spcr.write_u8(36, serial.interface_type);
    let (address_space, address) = match serial.registers {
        X86SerialRegisters::Port { base, .. } => (1, u64::from(base)),
        X86SerialRegisters::Mmio { base, .. } => (0, u64::from(base)),
    };
    spcr.write_u8(40, address_space);
    spcr.write_u8(41, 8);
    spcr.write_u8(42, 0);
    spcr.write_u8(43, 1);
    spcr.write_u64(44, address);
    spcr.write_u8(52, 2); // I/O APIC interrupt.
    spcr.write_u8(53, serial.irq as u8);
    spcr.write_u32(54, serial.irq);
    spcr.write_u8(58, 7); // 115200 baud.
    spcr.write_u8(59, 0); // No parity.
    spcr.write_u8(60, 1); // One stop bit.
    spcr.write_u8(61, 0);
    spcr.write_u8(62, 0);
    spcr.write_u16(64, u16::MAX);
    spcr.write_u16(66, u16::MAX);
    spcr.write_u32(76, serial.clock_hz);
    spcr.write_u32(80, 115_200);
    spcr.write_u16(84, namespace_length as u16);
    spcr.write_u16(86, INFO_SIZE as u16);
    spcr.write_bytes(88, namespace.as_bytes());
    spcr.write_u8(88 + namespace.len(), 0);
    spcr.as_slice().to_vec()
}

pub(super) const fn oem_id() -> [u8; 6] {
    OEM_ID
}

pub(super) const fn oem_table_id() -> [u8; 8] {
    OEM_TABLE_ID
}

pub(super) const fn oem_revision() -> u32 {
    OEM_REVISION
}
