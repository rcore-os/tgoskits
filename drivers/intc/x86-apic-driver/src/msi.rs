//! LAPIC MSI composition.
//!
//! The local APIC delivers MSI/MSI-X writes whose address encodes the
//! destination APIC id and whose data word carries the vector; this module
//! only composes those two values. Vector allocation, IRQ domains, and the
//! `rdif-msi` provider implementation stay in the OS glue, mirroring how the
//! GIC ITS provider lives in the platform layer.

/// A composed MSI address/data pair for one vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiTarget {
    /// Value to program into the message-address register.
    pub address: u64,
    /// Value to program into the message-data register.
    pub data: u32,
}

/// Composes the MSI address/data pair that delivers `vector` to
/// `destination_apic_id` (Intel SDM Vol. 3, "Message Signal Interrupts":
/// address base `0xFEE0_0000` with the destination id in bits 19:12, data
/// word carrying the vector).
pub fn compose_msi_message(vector: u8, destination_apic_id: u8) -> MsiTarget {
    MsiTarget {
        address: MSI_ADDRESS_BASE | (u64::from(destination_apic_id) << 12),
        data: u32::from(vector),
    }
}

const MSI_ADDRESS_BASE: u64 = 0xfee0_0000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_encodes_fixed_apic_destination_and_vector() {
        let message = compose_msi_message(0x81, 7);

        assert_eq!(message.address, MSI_ADDRESS_BASE | (7 << 12));
        assert_eq!(message.data, 0x81);
    }
}
