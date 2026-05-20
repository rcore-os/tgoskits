//! Minimal Intel MultiProcessor table for x86 Linux direct boot.

use alloc::vec::Vec;

use super::linux::X86LinuxRange;

pub const MP_TABLE_GPA: usize = 0x9f800;
pub const MP_TABLE_SIZE: usize = 0x800;

const MP_CONFIG_GPA: usize = MP_TABLE_GPA;
const MP_FLOATING_POINTER_GPA: usize = 0x9fc00;
const MP_FLOATING_POINTER_OFFSET: usize = MP_FLOATING_POINTER_GPA - MP_TABLE_GPA;

const LOCAL_APIC_ADDR: u32 = 0xfee0_0000;
const IO_APIC_ADDR: u32 = 0xfec0_0000;

const BSP_APIC_ID: u8 = 0;
const IO_APIC_ID: u8 = 1;
const APIC_VERSION: u8 = 0x14;
const IO_APIC_VERSION: u8 = 0x11;

const BUS_ID_PCI: u8 = 0;
const BUS_ID_ISA: u8 = 1;

/// Returns the reserved guest physical range occupied by the MP table.
pub const fn reserved_range() -> X86LinuxRange {
    X86LinuxRange::new(MP_TABLE_GPA, MP_TABLE_SIZE)
}

/// Builds a minimal MP floating pointer and MP config table.
pub fn build() -> [u8; MP_TABLE_SIZE] {
    let mut image = [0u8; MP_TABLE_SIZE];
    let config = build_config_table();
    image[..config.len()].copy_from_slice(&config);

    let floating = build_floating_pointer();
    image[MP_FLOATING_POINTER_OFFSET..MP_FLOATING_POINTER_OFFSET + floating.len()]
        .copy_from_slice(&floating);

    image
}

fn build_floating_pointer() -> [u8; 16] {
    let mut data = [0u8; 16];
    data[0..4].copy_from_slice(b"_MP_");
    data[4..8].copy_from_slice(&(MP_CONFIG_GPA as u32).to_le_bytes());
    data[8] = 1; // length in 16-byte units
    data[9] = 4; // MP spec revision 1.4
    data[10] = checksum(&data);
    data
}

fn build_config_table() -> Vec<u8> {
    let mut entries = Vec::new();
    push_processor_entry(&mut entries);
    push_bus_entry(&mut entries, BUS_ID_PCI, b"PCI   ");
    push_bus_entry(&mut entries, BUS_ID_ISA, b"ISA   ");
    push_io_apic_entry(&mut entries);
    push_isa_interrupt_entries(&mut entries);
    push_pci_interrupt_entries(&mut entries);

    let mut table = Vec::with_capacity(44 + entries.len());
    table.extend_from_slice(b"PCMP");
    table.extend_from_slice(&0u16.to_le_bytes());
    table.push(4); // MP spec revision 1.4
    table.push(0); // checksum, patched below
    table.extend_from_slice(b"AXVISOR ");
    table.extend_from_slice(b"X86LINUX    ");
    table.extend_from_slice(&0u32.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.extend_from_slice(&(entry_count() as u16).to_le_bytes());
    table.extend_from_slice(&LOCAL_APIC_ADDR.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.push(0);
    table.push(0);
    table.extend_from_slice(&entries);

    let len = table.len() as u16;
    table[4..6].copy_from_slice(&len.to_le_bytes());
    table[7] = checksum(&table);
    table
}

fn entry_count() -> usize {
    1 + 2 + 1 + 16 + 16
}

fn push_processor_entry(entries: &mut Vec<u8>) {
    entries.push(0);
    entries.push(BSP_APIC_ID);
    entries.push(APIC_VERSION);
    entries.push(0x03); // enabled + BSP
    entries.extend_from_slice(&0x0000_0600u32.to_le_bytes());
    entries.extend_from_slice(&0x0000_0201u32.to_le_bytes());
    entries.extend_from_slice(&[0; 8]);
}

fn push_bus_entry(entries: &mut Vec<u8>, bus_id: u8, bus_type: &[u8; 6]) {
    entries.push(1);
    entries.push(bus_id);
    entries.extend_from_slice(bus_type);
}

fn push_io_apic_entry(entries: &mut Vec<u8>) {
    entries.push(2);
    entries.push(IO_APIC_ID);
    entries.push(IO_APIC_VERSION);
    entries.push(0x01); // enabled
    entries.extend_from_slice(&IO_APIC_ADDR.to_le_bytes());
}

fn push_isa_interrupt_entries(entries: &mut Vec<u8>) {
    // Keep legacy ISA IRQs identity-routed to the IOAPIC. IRQ0 is timer and
    // IRQ4 is COM1; both are useful during early Linux bring-up diagnostics.
    for irq in 0u8..16 {
        push_interrupt_entry(entries, 0, BUS_ID_ISA, irq, irq);
    }
}

fn push_pci_interrupt_entries(entries: &mut Vec<u8>) {
    // QEMU q35 exposes the host rootfs virtio-blk as 00:03.0 in the current
    // smoke setup. Add conservative INTx routing for the first few slots so
    // Linux can build PCI IRQ routing before a fuller virtual IOAPIC exists.
    for dev in 0u8..4 {
        for pin in 0u8..4 {
            let source_irq = (dev << 2) | pin;
            let intin = 16 + ((dev + pin) & 3);
            push_interrupt_entry(entries, 0, BUS_ID_PCI, source_irq, intin);
        }
    }
}

fn push_interrupt_entry(
    entries: &mut Vec<u8>,
    interrupt_type: u8,
    source_bus_id: u8,
    source_bus_irq: u8,
    dest_io_apic_intin: u8,
) {
    entries.push(3);
    entries.push(interrupt_type);
    entries.extend_from_slice(&0u16.to_le_bytes());
    entries.push(source_bus_id);
    entries.push(source_bus_irq);
    entries.push(IO_APIC_ID);
    entries.push(dest_io_apic_intin);
}

fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_mp_table_checksums() {
        let image = build();
        let config_len = u16::from_le_bytes([image[4], image[5]]) as usize;
        assert_eq!(&image[..4], b"PCMP");
        assert_eq!(
            image[..config_len]
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
            0
        );

        let fp = &image[MP_FLOATING_POINTER_OFFSET..MP_FLOATING_POINTER_OFFSET + 16];
        assert_eq!(&fp[..4], b"_MP_");
        assert_eq!(fp.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)), 0);
        assert_eq!(
            u32::from_le_bytes([fp[4], fp[5], fp[6], fp[7]]) as usize,
            MP_CONFIG_GPA
        );
    }
}
