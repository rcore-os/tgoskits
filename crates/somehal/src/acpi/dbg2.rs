use acpi::{AcpiTable, sdt::SdtHeader};

#[derive(Debug, Clone)]
pub struct Dbg2 {
    pub header: SdtHeader,
}

unsafe impl AcpiTable for Dbg2 {
    const SIGNATURE: acpi::sdt::Signature = acpi::sdt::Signature::DBG2;

    fn header(&self) -> &acpi::sdt::SdtHeader {
        &self.header
    }
}
