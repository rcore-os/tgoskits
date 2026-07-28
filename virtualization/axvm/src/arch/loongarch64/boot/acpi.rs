//! LoongArch UEFI ACPI/AML blobs exposed through fw_cfg.
//!
//! `axdevice::FwCfg` is only the transport.  This module owns the guest-visible
//! LoongArch platform description so architecture policy does not leak into the
//! reusable device crate.

use alloc::{format, vec, vec::Vec};

use axdevice::{FwCfgAcpiBlobs, FwCfgRamRegion};

use super::GuestPlatform;

const ACPI_TABLE_FILE: &str = "etc/acpi/tables";
const ACPI_RSDP_FILE: &str = "etc/acpi/rsdp";
const ACPI_OEM_ID: &[u8; 6] = b"BOCHS ";
const ACPI_OEM_TABLE_ID: &[u8; 8] = b"BXPC    ";

const QEMU_LOADER_CMD_ALLOCATE: u32 = 1;
const QEMU_LOADER_CMD_ADD_POINTER: u32 = 2;
const QEMU_LOADER_CMD_ADD_CHECKSUM: u32 = 3;
const QEMU_LOADER_ALLOC_HIGH: u8 = 1;
const QEMU_LOADER_ALLOC_FSEG: u8 = 2;
const QEMU_LOADER_ENTRY_SIZE: usize = 128;

const VIRT_PCI_CFG_BASE: u64 = 0x2000_0000;
const VIRT_PCI_CFG_SIZE: u64 = 0x0800_0000;

#[derive(Clone, Copy, Debug)]
pub(super) struct LoongArchFwCfgSerialConfig {
    pub(super) base: u64,
    pub(super) size: u64,
    pub(super) irq: u8,
    pub(super) clock_hz: u32,
    pub(super) baud: u32,
}

impl Default for LoongArchFwCfgSerialConfig {
    fn default() -> Self {
        Self {
            base: 0x1fe0_01e0,
            size: 0x100,
            irq: 66,
            clock_hz: 100_000_000,
            baud: 115_200,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LoongArchFwCfgPciConfig {
    pub(super) ecam_base: u64,
    pub(super) ecam_size: u64,
    pub(super) mmio_base: u64,
    pub(super) mmio_size: u64,
    pub(super) io_base: u64,
    pub(super) io_size: u32,
    pub(super) intx_base: u8,
}

impl Default for LoongArchFwCfgPciConfig {
    fn default() -> Self {
        Self {
            ecam_base: VIRT_PCI_CFG_BASE,
            ecam_size: VIRT_PCI_CFG_SIZE,
            mmio_base: 0x4000_0000,
            mmio_size: 0x4000_0000,
            io_base: 0x1800_0000,
            io_size: 0x0001_0000,
            intx_base: 80,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LoongArchFwCfgInterruptConfig {
    pub(super) eiointc_irq: u8,
    pub(super) pch_msi_base: u64,
    pub(super) pch_msi_start: u32,
    pub(super) pch_msi_count: u32,
    pub(super) pch_pic_base: u64,
    pub(super) pch_pic_size: u16,
    pub(super) pch_pic_gsi_base: u16,
}

impl Default for LoongArchFwCfgInterruptConfig {
    fn default() -> Self {
        Self {
            eiointc_irq: 3,
            pch_msi_base: 0x2ff0_0000,
            pch_msi_start: 0x40,
            pch_msi_count: 0xc0,
            pch_pic_base: 0x1000_0000,
            pch_pic_size: 0x1000,
            pch_pic_gsi_base: 0x40,
        }
    }
}

pub(super) fn serial_config(platform: &GuestPlatform) -> LoongArchFwCfgSerialConfig {
    LoongArchFwCfgSerialConfig {
        base: platform.serial.mmio.base,
        size: platform.serial.mmio.size,
        irq: (platform.interrupt.acpi_gsi_base + platform.serial.irq) as u8,
        clock_hz: platform.serial.clock_hz,
        baud: platform.serial.baud,
    }
}

pub(super) fn pci_config(platform: &GuestPlatform) -> LoongArchFwCfgPciConfig {
    LoongArchFwCfgPciConfig {
        ecam_base: platform.pci.ecam.base,
        ecam_size: platform.pci.ecam.size,
        mmio_base: platform.pci.mmio.base,
        mmio_size: platform.pci.mmio.size,
        io_base: platform.pci.io_base,
        io_size: platform.pci.io_size as u32,
        intx_base: (platform.interrupt.acpi_gsi_base + platform.pci.intx_base) as u8,
    }
}

pub(super) fn interrupt_config(platform: &GuestPlatform) -> LoongArchFwCfgInterruptConfig {
    LoongArchFwCfgInterruptConfig {
        eiointc_irq: platform.interrupt.eiointc_irq as u8,
        pch_msi_base: platform.interrupt.pch_msi.base,
        pch_msi_start: platform.interrupt.acpi_msi_start,
        pch_msi_count: platform.interrupt.acpi_msi_count,
        pch_pic_base: platform.interrupt.pch_pic.base,
        pch_pic_size: platform.interrupt.pch_pic.size as u16,
        pch_pic_gsi_base: platform.interrupt.acpi_gsi_base as u16,
    }
}

pub(super) fn build(
    cpu_num: u16,
    platform: &GuestPlatform,
    srat_regions: &[FwCfgRamRegion],
) -> FwCfgAcpiBlobs {
    let serial = serial_config(platform);
    let pci = pci_config(platform);
    let interrupt = interrupt_config(platform);
    build_acpi(cpu_num, srat_regions, &serial, &pci, &interrupt)
}

fn build_acpi(
    cpu_num: u16,
    srat_regions: &[FwCfgRamRegion],
    serial: &LoongArchFwCfgSerialConfig,
    pci: &LoongArchFwCfgPciConfig,
    interrupt: &LoongArchFwCfgInterruptConfig,
) -> FwCfgAcpiBlobs {
    let mut tables = Vec::new();
    let mut loader = Vec::new();

    push_loader_allocate(&mut loader, ACPI_TABLE_FILE, 64, QEMU_LOADER_ALLOC_HIGH);

    let facs = tables.len() as u32;
    build_facs(&mut tables);

    let dsdt = tables.len() as u32;
    build_dsdt(&mut tables, serial, pci);
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        dsdt as usize,
        table_len(&tables, dsdt),
    );

    let fadt = tables.len() as u32;
    build_fadt(&mut tables);
    write_le_at(&mut tables, facs as u64, fadt as usize + 36, 4);
    push_loader_add_pointer(
        &mut loader,
        ACPI_TABLE_FILE,
        fadt + 36,
        4,
        ACPI_TABLE_FILE,
        facs,
    );
    write_le_at(&mut tables, dsdt as u64, fadt as usize + 40, 4);
    push_loader_add_pointer(
        &mut loader,
        ACPI_TABLE_FILE,
        fadt + 40,
        4,
        ACPI_TABLE_FILE,
        dsdt,
    );
    write_le_at(&mut tables, dsdt as u64, fadt as usize + 140, 8);
    push_loader_add_pointer(
        &mut loader,
        ACPI_TABLE_FILE,
        fadt + 140,
        8,
        ACPI_TABLE_FILE,
        dsdt,
    );
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        fadt as usize,
        table_len(&tables, fadt),
    );

    let madt = tables.len() as u32;
    build_madt(&mut tables, cpu_num, interrupt);
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        madt as usize,
        table_len(&tables, madt),
    );

    let srat = tables.len() as u32;
    build_srat(&mut tables, cpu_num, srat_regions);
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        srat as usize,
        table_len(&tables, srat),
    );

    let spcr = tables.len() as u32;
    build_spcr(&mut tables, serial);
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        spcr as usize,
        table_len(&tables, spcr),
    );

    let mcfg = tables.len() as u32;
    build_mcfg(&mut tables, pci);
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        mcfg as usize,
        table_len(&tables, mcfg),
    );

    let table_offsets = [fadt, madt, srat, spcr, mcfg];
    let rsdt = tables.len() as u32;
    build_rsdt(&mut tables, &table_offsets);
    for (idx, table_offset) in table_offsets.iter().enumerate() {
        write_le_at(
            &mut tables,
            *table_offset as u64,
            rsdt as usize + 36 + (idx * 4),
            4,
        );
        push_loader_add_pointer(
            &mut loader,
            ACPI_TABLE_FILE,
            rsdt + 36 + (idx as u32 * 4),
            4,
            ACPI_TABLE_FILE,
            *table_offset,
        );
    }
    push_loader_add_checksum(
        &mut loader,
        ACPI_TABLE_FILE,
        rsdt as usize,
        table_len(&tables, rsdt),
    );

    let mut rsdp = Vec::new();
    push_loader_allocate(&mut loader, ACPI_RSDP_FILE, 16, QEMU_LOADER_ALLOC_FSEG);
    build_rsdp(&mut rsdp);
    write_le_at(&mut rsdp, rsdt as u64, 16, 4);
    push_loader_add_pointer(&mut loader, ACPI_RSDP_FILE, 16, 4, ACPI_TABLE_FILE, rsdt);
    push_loader_add_checksum(&mut loader, ACPI_RSDP_FILE, 0, 20);

    FwCfgAcpiBlobs {
        tables,
        rsdp,
        loader,
    }
}

fn table_len(tables: &[u8], offset: u32) -> usize {
    u32::from_le_bytes(
        tables[offset as usize + 4..offset as usize + 8]
            .try_into()
            .unwrap(),
    ) as usize
}

fn build_facs(tables: &mut Vec<u8>) {
    tables.extend_from_slice(b"FACS");
    push_le(tables, 64, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    tables.resize(tables.len() + 40, 0);
}

fn build_dsdt(
    tables: &mut Vec<u8>,
    serial: &LoongArchFwCfgSerialConfig,
    pci: &LoongArchFwCfgPciConfig,
) {
    let start = begin_acpi_table(tables, b"DSDT", 1);
    tables.extend_from_slice(&build_loongarch_dsdt_aml(serial, pci));
    end_acpi_table(tables, start);
}

fn build_fadt(tables: &mut Vec<u8>) {
    let start = begin_acpi_table(tables, b"FACP", 5);
    push_le(tables, 0, 4);
    push_le(tables, 0, 4);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 2);
    push_le(tables, 0, 4);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    for _ in 0..8 {
        push_le(tables, 0, 4);
    }
    for _ in 0..8 {
        push_le(tables, 0, 1);
    }
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    for _ in 0..5 {
        push_le(tables, 0, 1);
    }
    push_le(tables, 0, 2);
    push_le(tables, 0, 1);
    push_le(tables, (1u64 << 10) | (1u64 << 20), 4);
    push_gas(tables, 0, 8, 0, 1, 0x100e_001e);
    push_le(tables, 0x42, 1);
    push_le(tables, 0, 3);
    push_le(tables, 0, 8);
    push_le(tables, 0, 8);
    for _ in 0..8 {
        push_gas(tables, 0, 0, 0, 0, 0);
    }
    push_gas(tables, 0, 8, 0, 1, 0x100e_001c);
    push_gas(tables, 0, 8, 0, 1, 0x100e_001d);
    end_acpi_table(tables, start);
}

fn build_madt(tables: &mut Vec<u8>, cpu_num: u16, interrupt: &LoongArchFwCfgInterruptConfig) {
    let start = begin_acpi_table(tables, b"APIC", 1);
    push_le(tables, 0, 4);
    push_le(tables, 1, 4);

    for cpu_id in 0..cpu_num {
        push_le(tables, 17, 1);
        push_le(tables, 15, 1);
        push_le(tables, 1, 1);
        push_le(tables, cpu_id as u64, 4);
        push_le(tables, cpu_id as u64, 4);
        push_le(tables, 1, 4);
    }

    push_le(tables, 20, 1);
    push_le(tables, 13, 1);
    push_le(tables, 1, 1);
    push_le(tables, interrupt.eiointc_irq as u64, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0xffff, 8);

    push_le(tables, 21, 1);
    push_le(tables, 19, 1);
    push_le(tables, 1, 1);
    push_le(tables, interrupt.pch_msi_base, 8);
    push_le(tables, interrupt.pch_msi_start as u64, 4);
    push_le(tables, interrupt.pch_msi_count as u64, 4);

    push_le(tables, 22, 1);
    push_le(tables, 17, 1);
    push_le(tables, 1, 1);
    push_le(tables, interrupt.pch_pic_base, 8);
    push_le(tables, interrupt.pch_pic_size as u64, 2);
    push_le(tables, 0, 2);
    push_le(tables, interrupt.pch_pic_gsi_base as u64, 2);

    end_acpi_table(tables, start);
}

fn build_srat(tables: &mut Vec<u8>, cpu_num: u16, ram_regions: &[FwCfgRamRegion]) {
    let start = begin_acpi_table(tables, b"SRAT", 1);
    push_le(tables, 1, 4);
    push_le(tables, 0, 8);

    for cpu_id in 0..cpu_num {
        push_le(tables, 0, 1);
        push_le(tables, 16, 1);
        push_le(tables, 0, 1);
        push_le(tables, cpu_id as u64, 1);
        push_le(tables, 1, 4);
        push_le(tables, 0, 1);
        push_le(tables, 0, 3);
        push_le(tables, 0, 4);
    }

    for region in ram_regions {
        if region.size != 0 {
            push_srat_memory(tables, region.base, region.size);
        }
    }

    end_acpi_table(tables, start);
}

fn push_srat_memory(tables: &mut Vec<u8>, base: u64, length: u64) {
    push_le(tables, 1, 1);
    push_le(tables, 40, 1);
    push_le(tables, 0, 4);
    push_le(tables, 0, 2);
    push_le(tables, base, 4);
    push_le(tables, base >> 32, 4);
    push_le(tables, length, 4);
    push_le(tables, length >> 32, 4);
    push_le(tables, 0, 4);
    push_le(tables, 1, 4);
    push_le(tables, 0, 8);
}

fn build_spcr(tables: &mut Vec<u8>, serial: &LoongArchFwCfgSerialConfig) {
    let start = begin_acpi_table(tables, b"SPCR", 2);
    push_le(tables, 0, 1);
    push_le(tables, 0, 3);
    push_gas(tables, 0, 8, 0, 1, serial.base);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, serial.irq as u64, 4);
    push_le(tables, 7, 1);
    push_le(tables, 0, 1);
    push_le(tables, 1, 1);
    push_le(tables, 0, 1);
    push_le(tables, 3, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0xffff, 2);
    push_le(tables, 0xffff, 2);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 1);
    push_le(tables, 0, 4);
    push_le(tables, 0, 1);
    push_le(tables, 0, 4);
    push_le(tables, serial.clock_hz as u64, 4);
    push_le(tables, serial.baud as u64, 4);
    push_le(tables, 0, 2);
    push_le(tables, 0, 2);
    end_acpi_table(tables, start);
}

fn build_mcfg(tables: &mut Vec<u8>, pci: &LoongArchFwCfgPciConfig) {
    let start = begin_acpi_table(tables, b"MCFG", 1);
    push_le(tables, 0, 8);
    push_le(tables, pci.ecam_base, 8);
    push_le(tables, 0, 2);
    push_le(tables, 0, 1);
    push_le(tables, (pci.ecam_size - 1) >> 20, 1);
    push_le(tables, 0, 4);
    end_acpi_table(tables, start);
}

fn build_rsdt(tables: &mut Vec<u8>, table_offsets: &[u32]) {
    let start = begin_acpi_table(tables, b"RSDT", 1);
    for _ in table_offsets {
        push_le(tables, 0, 4);
    }
    end_acpi_table(tables, start);
}

fn build_rsdp(rsdp: &mut Vec<u8>) {
    rsdp.extend_from_slice(b"RSD PTR ");
    push_le(rsdp, 0, 1);
    rsdp.extend_from_slice(ACPI_OEM_ID);
    push_le(rsdp, 0, 1);
    push_le(rsdp, 0, 4);
}

fn begin_acpi_table(tables: &mut Vec<u8>, signature: &[u8; 4], revision: u8) -> usize {
    let start = tables.len();
    tables.extend_from_slice(signature);
    push_le(tables, 0, 4);
    push_le(tables, revision as u64, 1);
    push_le(tables, 0, 1);
    tables.extend_from_slice(ACPI_OEM_ID);
    tables.extend_from_slice(ACPI_OEM_TABLE_ID);
    push_le(tables, 1, 4);
    tables.extend_from_slice(b"BXPC");
    push_le(tables, 1, 4);
    start
}

fn end_acpi_table(tables: &mut [u8], start: usize) {
    let length = (tables.len() - start) as u32;
    tables[start + 4..start + 8].copy_from_slice(&length.to_le_bytes());
}

fn build_loongarch_dsdt_aml(
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

fn push_gas(out: &mut Vec<u8>, space: u8, bit_width: u8, bit_offset: u8, access: u8, addr: u64) {
    out.push(space);
    out.push(bit_width);
    out.push(bit_offset);
    out.push(access);
    push_le(out, addr, 8);
}

fn push_loader_allocate(out: &mut Vec<u8>, file: &str, align: u32, zone: u8) {
    let mut entry = [0u8; QEMU_LOADER_ENTRY_SIZE];
    entry[0..4].copy_from_slice(&QEMU_LOADER_CMD_ALLOCATE.to_le_bytes());
    write_loader_file(&mut entry[4..60], file);
    entry[60..64].copy_from_slice(&align.to_le_bytes());
    entry[64] = zone;
    out.extend_from_slice(&entry);
}

fn push_loader_add_pointer(
    out: &mut Vec<u8>,
    pointer_file: &str,
    pointer_offset: u32,
    pointer_size: u8,
    pointee_file: &str,
    pointee_offset: u32,
) {
    let mut entry = [0u8; QEMU_LOADER_ENTRY_SIZE];
    entry[0..4].copy_from_slice(&QEMU_LOADER_CMD_ADD_POINTER.to_le_bytes());
    write_loader_file(&mut entry[4..60], pointer_file);
    write_loader_file(&mut entry[60..116], pointee_file);
    entry[116..120].copy_from_slice(&pointer_offset.to_le_bytes());
    entry[120] = pointer_size;
    let _ = pointee_offset;
    out.extend_from_slice(&entry);
}

fn push_loader_add_checksum(out: &mut Vec<u8>, file: &str, start: usize, length: usize) {
    let mut entry = [0u8; QEMU_LOADER_ENTRY_SIZE];
    entry[0..4].copy_from_slice(&QEMU_LOADER_CMD_ADD_CHECKSUM.to_le_bytes());
    write_loader_file(&mut entry[4..60], file);
    entry[60..64].copy_from_slice(&((start + 9) as u32).to_le_bytes());
    entry[64..68].copy_from_slice(&(start as u32).to_le_bytes());
    entry[68..72].copy_from_slice(&(length as u32).to_le_bytes());
    out.extend_from_slice(&entry);
}

fn write_loader_file(dst: &mut [u8], file: &str) {
    let bytes = file.as_bytes();
    let len = core::cmp::min(bytes.len(), dst.len().saturating_sub(1));
    dst[..len].copy_from_slice(&bytes[..len]);
}

fn write_le_at(out: &mut [u8], value: u64, offset: usize, size: u8) {
    let bytes = value.to_le_bytes();
    out[offset..offset + size as usize].copy_from_slice(&bytes[..size as usize]);
}

fn push_le(out: &mut Vec<u8>, value: u64, size: usize) {
    out.extend_from_slice(&value.to_le_bytes()[..size]);
}
