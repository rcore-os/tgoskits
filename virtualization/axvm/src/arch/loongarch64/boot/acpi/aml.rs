//! LoongArch platform AML generation.

use std::{format, vec, vec::Vec};

use super::config::{LoongArchFwCfgPciConfig, LoongArchFwCfgSerialConfig};

pub(super) fn build_loongarch_dsdt_aml(
    serial: &LoongArchFwCfgSerialConfig,
    pci: &LoongArchFwCfgPciConfig,
) -> Vec<u8> {
    let mut scope_body = Vec::new();
    scope_body.extend(aml_device("COMA", build_coma_aml(serial)));
    scope_body.extend(aml_device("PCI0", build_pci0_aml(pci)));

    let mut aml = Vec::new();
    aml.extend(aml_scope("_SB_", scope_body));
    aml.extend(aml_name_decl(
        "_S5_",
        aml_package(&[aml_int(5), aml_int(0), aml_int(0), aml_int(0)]),
    ));
    aml
}

fn build_coma_aml(serial: &LoongArchFwCfgSerialConfig) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(aml_name_decl("_HID", aml_string("PNP0501")));
    body.extend(aml_name_decl("_UID", aml_int(0)));
    body.extend(aml_name_decl("_CCA", aml_int(1)));
    body.extend(aml_name_decl("_CRS", serial_crs_aml(serial)));
    body.extend_from_slice(&[
        0x08, 0x5f, 0x44, 0x53, 0x44, 0x12, 0x32, 0x02, 0x11, 0x13, 0x0a, 0x10, 0x14, 0xd8, 0xff,
        0xda, 0xba, 0x6e, 0x8c, 0x4d, 0x8a, 0x91, 0xbc, 0x9b, 0xbf, 0x4a, 0xa3, 0x01, 0x12, 0x1b,
        0x01, 0x12, 0x18, 0x02, 0x0d, 0x63, 0x6c, 0x6f, 0x63, 0x6b, 0x2d, 0x66, 0x72, 0x65, 0x71,
        0x75, 0x65, 0x6e, 0x63, 0x79, 0x00, 0x0c, 0x00, 0xe1, 0xf5, 0x05,
    ]);
    body
}

fn build_pci0_aml(pci: &LoongArchFwCfgPciConfig) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(aml_name_decl("_HID", aml_string("PNP0A08")));
    body.extend(aml_name_decl("_CID", aml_string("PNP0A03")));
    body.extend(aml_name_decl("_SEG", aml_int(0)));
    body.extend(aml_name_decl("_BBN", aml_int(0)));
    body.extend(aml_name_decl("_UID", aml_int(0)));
    body.extend(aml_name_decl("_CCA", aml_int(1)));
    body.extend(aml_name_decl("_PRT", pci_route_package_aml()));

    for gsi in 0..4 {
        body.extend(aml_device(
            &format!("GSI{gsi}"),
            build_gsi_link_aml(pci, gsi),
        ));
    }

    body.extend(aml_method("_CBA", 0, aml_return(aml_int(pci.ecam_base))));
    body.extend(aml_name_decl("_CRS", pci_crs_aml(pci)));
    body.extend(aml_device("RES0", build_pci_res0_aml(pci)));
    body
}

fn pci_route_package_aml() -> Vec<u8> {
    const PCI_SLOT_MAX: usize = 32;
    const PCI_NUM_PINS: usize = 4;

    let mut entries = Vec::new();
    for slot in 0..PCI_SLOT_MAX {
        for pin in 0..PCI_NUM_PINS {
            let gsi = (pin + slot) % PCI_NUM_PINS;
            let address = ((slot as u64) << 16) | 0xffff;
            entries.push(aml_package(&[
                aml_int(address),
                aml_int(pin as u64),
                aml_name_ref(&format!("GSI{gsi}")),
                aml_int(0),
            ]));
        }
    }
    aml_package_with_count(entries, (PCI_SLOT_MAX * PCI_NUM_PINS) as u8)
}

fn build_gsi_link_aml(pci: &LoongArchFwCfgPciConfig, gsi: usize) -> Vec<u8> {
    let irq = pci.intx_base + gsi as u8;
    let mut body = Vec::new();
    body.extend(aml_name_decl("_HID", aml_string("PNP0C0F")));
    body.extend(aml_name_decl("_UID", aml_int(gsi as u64)));
    body.extend(aml_name_decl("_PRS", interrupt_crs_aml(irq, false)));
    body.extend(aml_name_decl("_CRS", interrupt_crs_aml(irq, false)));
    body.extend(aml_method("_SRS", 1, Vec::new()));
    body
}

fn build_pci_res0_aml(pci: &LoongArchFwCfgPciConfig) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(aml_name_decl("_HID", aml_string("PNP0C02")));
    body.extend(aml_name_decl("_CRS", pci_res0_crs_aml(pci)));
    body
}

fn aml_scope(name: &str, body: Vec<u8>) -> Vec<u8> {
    let mut content = aml_name_ref(name);
    content.extend(body);
    aml_pkg_op(&[0x10], content)
}

fn aml_device(name: &str, body: Vec<u8>) -> Vec<u8> {
    let mut content = aml_name_ref(name);
    content.extend(body);
    aml_pkg_op(&[0x5b, 0x82], content)
}

fn aml_method(name: &str, arg_count: u8, body: Vec<u8>) -> Vec<u8> {
    let mut content = aml_name_ref(name);
    content.push(arg_count & 0x7);
    content.extend(body);
    aml_pkg_op(&[0x14], content)
}

fn aml_pkg_op(opcode: &[u8], content: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(opcode);
    out.extend(aml_pkg_len(content.len()));
    out.extend(content);
    out
}

fn aml_pkg_len(content_len: usize) -> Vec<u8> {
    for len_len in 1..=4 {
        let total_len = content_len + len_len;
        let max_len = 1usize << (4 + 8 * (len_len - 1));
        if total_len < max_len {
            if len_len == 1 {
                return vec![total_len as u8];
            }
            let mut bytes = Vec::with_capacity(len_len);
            bytes.push((((len_len - 1) as u8) << 6) | ((total_len as u8) & 0x0f));
            let mut remaining = total_len >> 4;
            for _ in 1..len_len {
                bytes.push((remaining & 0xff) as u8);
                remaining >>= 8;
            }
            return bytes;
        }
    }
    unreachable!("AML package is too large")
}

fn aml_name_decl(name: &str, value: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x08);
    out.extend(aml_name_ref(name));
    out.extend(value);
    out
}

fn aml_name_ref(name: &str) -> Vec<u8> {
    if name == "\\" {
        return vec![0x5c];
    }
    let bytes = name.as_bytes();
    assert_eq!(bytes.len(), 4, "AML short names must be 4 bytes");
    bytes.to_vec()
}

fn aml_string(value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 2);
    out.push(0x0d);
    out.extend_from_slice(value.as_bytes());
    out.push(0);
    out
}

fn aml_int(value: u64) -> Vec<u8> {
    match value {
        0 => vec![0x00],
        1 => vec![0x01],
        2..=0xff => vec![0x0a, value as u8],
        0x100..=0xffff => {
            let mut out = vec![0x0b];
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out
        }
        0x1_0000..=0xffff_ffff => {
            let mut out = vec![0x0c];
            out.extend_from_slice(&(value as u32).to_le_bytes());
            out
        }
        _ => {
            let mut out = vec![0x0e];
            out.extend_from_slice(&value.to_le_bytes());
            out
        }
    }
}

fn aml_package(elements: &[Vec<u8>]) -> Vec<u8> {
    aml_package_with_count(elements.to_vec(), elements.len() as u8)
}

fn aml_package_with_count(elements: Vec<Vec<u8>>, count: u8) -> Vec<u8> {
    let mut content = vec![count];
    for element in elements {
        content.extend(element);
    }
    aml_pkg_op(&[0x12], content)
}

fn aml_return(value: Vec<u8>) -> Vec<u8> {
    let mut out = vec![0xa4];
    out.extend(value);
    out
}

fn aml_buffer(bytes: &[u8]) -> Vec<u8> {
    let mut content = Vec::with_capacity(1 + bytes.len());
    content.extend(aml_int(bytes.len() as u64));
    content.extend_from_slice(bytes);
    aml_pkg_op(&[0x11], content)
}

fn aml_resource_template(resources: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for resource in resources {
        bytes.extend_from_slice(resource);
    }
    bytes.extend_from_slice(&[0x79, 0x00]);
    aml_buffer(&bytes)
}

fn word_bus_number_resource(min: u16, max: u16) -> Vec<u8> {
    let length = max.saturating_sub(min).saturating_add(1);
    let mut out = vec![0x88, 0x0d, 0x00, 0x02, 0x0c, 0x00, 0x00, 0x00];
    out.extend_from_slice(&min.to_le_bytes());
    out.extend_from_slice(&max.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&length.to_le_bytes());
    out
}

fn dword_io_resource(base: u64, size: u32) -> Vec<u8> {
    let max = size.saturating_sub(1);
    let mut out = vec![0x87, 0x17, 0x00, 0x01, 0x0c, 0x03];
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&max.to_le_bytes());
    out.extend_from_slice(&(base as u32).to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out
}

fn qword_memory_resource(base: u64, size: u64, prefetchable: bool, fixed: bool) -> Vec<u8> {
    let max = base.saturating_add(size).saturating_sub(1);
    let mut out = vec![
        0x8a,
        0x2b,
        0x00,
        0x00,
        if fixed { 0x0d } else { 0x0c },
        if prefetchable { 0x03 } else { 0x01 },
    ];
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&base.to_le_bytes());
    out.extend_from_slice(&max.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out
}

fn extended_interrupt_resource(irqs: &[u32], consumer: bool) -> Vec<u8> {
    let payload_len = 2 + core::mem::size_of_val(irqs);
    let mut out = vec![0x89];
    out.extend_from_slice(&(payload_len as u16).to_le_bytes());
    out.push(if consumer { 0x09 } else { 0x01 });
    out.push(irqs.len() as u8);
    for irq in irqs {
        out.extend_from_slice(&irq.to_le_bytes());
    }
    out
}

fn serial_crs_aml(serial: &LoongArchFwCfgSerialConfig) -> Vec<u8> {
    aml_resource_template(&[
        qword_memory_resource(serial.base, serial.size, false, true),
        extended_interrupt_resource(&[serial.irq as u32], true),
    ])
}

fn interrupt_crs_aml(irq: u8, shared: bool) -> Vec<u8> {
    vec![
        0x11,
        0x0e,
        0x0a,
        0x0b,
        0x89,
        0x06,
        0x00,
        0x01,
        if shared { 0x09 } else { 0x01 },
        irq,
        0x00,
        0x00,
        0x00,
        0x79,
        0x00,
    ]
}

fn pci_crs_aml(pci: &LoongArchFwCfgPciConfig) -> Vec<u8> {
    aml_resource_template(&[
        word_bus_number_resource(0, ((pci.ecam_size - 1) >> 20) as u16),
        dword_io_resource(pci.io_base, pci.io_size),
        qword_memory_resource(pci.mmio_base, pci.mmio_size, false, false),
    ])
}

fn pci_res0_crs_aml(pci: &LoongArchFwCfgPciConfig) -> Vec<u8> {
    aml_resource_template(&[qword_memory_resource(
        pci.ecam_base,
        pci.ecam_size,
        false,
        true,
    )])
}
