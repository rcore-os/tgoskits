//! Final ACPI table metadata and direct-boot image.

use std::vec::Vec;

use super::AcpiBuildError;

/// One standard ACPI table placed in guest memory or an fw_cfg file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcpiTableRecord {
    signature: [u8; 4],
    address: u64,
    length: usize,
}

impl AcpiTableRecord {
    pub(crate) const fn new(signature: [u8; 4], address: u64, length: usize) -> Self {
        Self {
            signature,
            address,
            length,
        }
    }

    #[cfg(test)]
    pub(crate) const fn signature(&self) -> [u8; 4] {
        self.signature
    }

    #[cfg(test)]
    pub(crate) const fn address(&self) -> u64 {
        self.address
    }

    #[cfg(test)]
    pub(crate) const fn length(&self) -> usize {
        self.length
    }
}

/// Logical table collection used for pointer closure and diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AcpiTableSet {
    tables: Vec<AcpiTableRecord>,
}

impl AcpiTableSet {
    pub(crate) const fn new() -> Self {
        Self { tables: Vec::new() }
    }

    pub(crate) fn add(&mut self, table: AcpiTableRecord) -> Result<(), AcpiBuildError> {
        if self
            .tables
            .iter()
            .any(|existing| existing.signature == table.signature)
        {
            return Err(AcpiBuildError::DuplicateTable {
                signature: table.signature,
            });
        }
        self.tables.push(table);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn find(&self, signature: [u8; 4]) -> Option<&AcpiTableRecord> {
        self.tables
            .iter()
            .find(|table| table.signature == signature)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &AcpiTableRecord> {
        self.tables.iter()
    }
}

/// ACPI bytes installed directly into guest physical memory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AcpiImage {
    load_gpa: u64,
    rsdp_gpa: u64,
    bytes: Vec<u8>,
    tables: AcpiTableSet,
}

impl AcpiImage {
    pub(crate) const fn new(
        load_gpa: u64,
        rsdp_gpa: u64,
        bytes: Vec<u8>,
        tables: AcpiTableSet,
    ) -> Self {
        Self {
            load_gpa,
            rsdp_gpa,
            bytes,
            tables,
        }
    }

    pub(crate) const fn load_gpa(&self) -> u64 {
        self.load_gpa
    }

    pub(crate) const fn rsdp_gpa(&self) -> u64 {
        self.rsdp_gpa
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) const fn tables(&self) -> &AcpiTableSet {
        &self.tables
    }
}
