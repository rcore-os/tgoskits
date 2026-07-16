//! ACPI firmware generated from finalized machine resources.

mod devices;
mod loongarch;
mod x86;

use alloc::vec::Vec;

use acpi_tables::Aml;
pub use loongarch::generate_loongarch_fw_cfg_acpi;
pub use x86::{X86AcpiConfig, generate_x86_acpi};

pub(super) const OEM_ID: [u8; 6] = *b"AXVISR";
pub(super) const OEM_TABLE_ID: [u8; 8] = *b"AXVIRT  ";
pub(super) const OEM_REVISION: u32 = 1;
pub(super) const TABLE_ALIGNMENT: usize = 64;
pub(super) const ACPI_HEADER_LENGTH: usize = 36;

#[derive(Clone, Copy, Debug)]
pub(super) struct AcpiTableLocation {
    signature: [u8; 4],
    offset: usize,
    length: usize,
}

/// One contiguous, address-resolved ACPI image ready for guest RAM.
#[derive(Clone, Debug)]
pub struct GeneratedAcpiImage {
    load_address: u64,
    bytes: Vec<u8>,
    tables: Vec<AcpiTableLocation>,
}

impl GeneratedAcpiImage {
    pub(super) const fn new(
        load_address: u64,
        bytes: Vec<u8>,
        tables: Vec<AcpiTableLocation>,
    ) -> Self {
        Self {
            load_address,
            bytes,
            tables,
        }
    }

    /// Returns the guest physical address of the RSDP at the image start.
    pub const fn rsdp_address(&self) -> u64 {
        self.load_address
    }

    /// Returns the guest physical address where the image begins.
    pub const fn load_address(&self) -> u64 {
        self.load_address
    }

    /// Returns the image size in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Returns whether the image is empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Returns the complete contiguous image.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns one SDT by its four-byte signature.
    pub fn table(&self, signature: [u8; 4]) -> Option<&[u8]> {
        self.tables
            .iter()
            .find(|table| table.signature == signature)
            .map(|table| &self.bytes[table.offset..table.offset + table.length])
    }

    /// Returns the guest physical address of one SDT.
    pub fn table_address(&self, signature: [u8; 4]) -> Option<u64> {
        self.tables
            .iter()
            .find(|table| table.signature == signature)
            .map(|table| self.load_address + table.offset as u64)
    }
}

pub(super) fn aml_bytes(value: &dyn Aml) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.to_aml_bytes(&mut bytes);
    bytes
}

pub(super) const fn location(signature: [u8; 4], offset: usize, bytes: &[u8]) -> AcpiTableLocation {
    AcpiTableLocation {
        signature,
        offset,
        length: bytes.len(),
    }
}
