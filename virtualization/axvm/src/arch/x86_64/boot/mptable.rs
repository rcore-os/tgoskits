//! Minimal Intel MultiProcessor table for x86 Linux direct boot.

use std::vec::Vec;

use super::linux::X86LinuxRange;

pub const MP_TABLE_GPA: usize = 0x9f800;
pub const MP_TABLE_SIZE: usize = 0x800;

const MP_CONFIG_GPA: usize = MP_TABLE_GPA;
const MP_FLOATING_POINTER_GPA: usize = 0x9fc00;
const MP_FLOATING_POINTER_OFFSET: usize = MP_FLOATING_POINTER_GPA - MP_TABLE_GPA;

const IO_APIC_ID: u8 = 1;
const APIC_VERSION: u8 = 0x14;
const IO_APIC_VERSION: u8 = 0x11;

const BUS_ID_PCI: u8 = 0;
const BUS_ID_ISA: u8 = 1;

const MP_IRQ_FLAGS_CONFORMING: u16 = 0;
const MP_IRQ_FLAGS_ACTIVE_LOW: u16 = 0x3;
const MP_IRQ_FLAGS_LEVEL_TRIGGERED: u16 = 0xc;
const PCI_INTX_IRQ_FLAGS: u16 = MP_IRQ_FLAGS_ACTIVE_LOW | MP_IRQ_FLAGS_LEVEL_TRIGGERED;

/// Returns the reserved guest physical range occupied by the MP table.
pub const fn reserved_range() -> X86LinuxRange {
    X86LinuxRange::new(MP_TABLE_GPA, MP_TABLE_SIZE)
}

/// Builds a minimal MP floating pointer and MP config table.
pub fn build(
    apic_ids: &[u8],
    local_apic_address: u32,
    io_apic_address: u32,
) -> [u8; MP_TABLE_SIZE] {
    let mut image = [0u8; MP_TABLE_SIZE];
    let config = build_config_table(apic_ids, local_apic_address, io_apic_address);
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

fn build_config_table(apic_ids: &[u8], local_apic_address: u32, io_apic_address: u32) -> Vec<u8> {
    let entries = config_entries(apic_ids, io_apic_address);
    let entries_len: usize = entries.iter().map(Vec::len).sum();

    let mut table = Vec::with_capacity(44 + entries_len);
    table.extend_from_slice(b"PCMP");
    table.extend_from_slice(&0u16.to_le_bytes());
    table.push(4); // MP spec revision 1.4
    table.push(0); // checksum, patched below
    table.extend_from_slice(b"AXVISOR ");
    table.extend_from_slice(b"X86LINUX    ");
    table.extend_from_slice(&0u32.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    table.extend_from_slice(&local_apic_address.to_le_bytes());
    table.extend_from_slice(&0u16.to_le_bytes());
    table.push(0);
    table.push(0);
    for entry in &entries {
        table.extend_from_slice(entry);
    }

    let len = table.len() as u16;
    table[4..6].copy_from_slice(&len.to_le_bytes());
    table[7] = checksum(&table);
    table
}

fn config_entries(apic_ids: &[u8], io_apic_address: u32) -> Vec<Vec<u8>> {
    let mut entries = apic_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, apic_id)| processor_entry(apic_id, index == 0))
        .collect::<Vec<_>>();
    entries.extend([
        bus_entry(BUS_ID_PCI, b"PCI   "),
        bus_entry(BUS_ID_ISA, b"ISA   "),
        io_apic_entry(io_apic_address),
    ]);
    push_isa_interrupt_entries(&mut entries);
    push_pci_interrupt_entries(&mut entries);
    entries
}

fn processor_entry(apic_id: u8, bsp: bool) -> Vec<u8> {
    let mut entry = Vec::with_capacity(20);
    entry.push(0);
    entry.push(apic_id);
    entry.push(APIC_VERSION);
    entry.push(0x01 | u8::from(bsp) << 1); // enabled, plus BSP on vCPU 0.
    entry.extend_from_slice(&0x0000_0600u32.to_le_bytes());
    entry.extend_from_slice(&0x0000_0201u32.to_le_bytes());
    entry.extend_from_slice(&[0; 8]);
    entry
}

fn bus_entry(bus_id: u8, bus_type: &[u8; 6]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(8);
    entry.push(1);
    entry.push(bus_id);
    entry.extend_from_slice(bus_type);
    entry
}

fn io_apic_entry(io_apic_address: u32) -> Vec<u8> {
    let mut entry = Vec::with_capacity(8);
    entry.push(2);
    entry.push(IO_APIC_ID);
    entry.push(IO_APIC_VERSION);
    entry.push(0x01); // enabled
    entry.extend_from_slice(&io_apic_address.to_le_bytes());
    entry
}

fn push_isa_interrupt_entries(entries: &mut Vec<Vec<u8>>) {
    // Keep legacy ISA IRQs identity-routed to the IOAPIC. IRQ0 is timer and
    // IRQ4 is COM1; both are useful during early Linux bring-up diagnostics.
    for irq in 0u8..16 {
        entries.push(interrupt_entry(
            0,
            MP_IRQ_FLAGS_CONFORMING,
            BUS_ID_ISA,
            irq,
            irq,
        ));
    }
}

fn push_pci_interrupt_entries(entries: &mut Vec<Vec<u8>>) {
    // QEMU q35 exposes the host rootfs virtio-blk as 00:03.0 in the current
    // smoke setup. Add enough INTx routing for Linux to build the PCI IRQ
    // table before a fuller virtual PCI IRQ router exists.
    for dev in 0u8..4 {
        for pin in 0u8..4 {
            let source_irq = (dev << 2) | pin;
            let intin = pci_intx_gsi(dev, pin);
            entries.push(interrupt_entry(
                0,
                PCI_INTX_IRQ_FLAGS,
                BUS_ID_PCI,
                source_irq,
                intin,
            ));
        }
    }
}

const fn pci_intx_gsi(dev: u8, pin: u8) -> u8 {
    // Match q35 PCI INTx swizzling in the guest MP table. The host IRQ can be
    // a different ACPI route; Axvisor registers that native host IRQ against
    // this guest GSI explicitly.
    16 + ((dev + pin) & 3)
}

fn interrupt_entry(
    interrupt_type: u8,
    flags: u16,
    source_bus_id: u8,
    source_bus_irq: u8,
    dest_io_apic_intin: u8,
) -> Vec<u8> {
    let mut entry = Vec::with_capacity(8);
    entry.push(3);
    entry.push(interrupt_type);
    entry.extend_from_slice(&flags.to_le_bytes());
    entry.push(source_bus_id);
    entry.push(source_bus_irq);
    entry.push(IO_APIC_ID);
    entry.push(dest_io_apic_intin);
    entry
}

fn checksum(bytes: &[u8]) -> u8 {
    0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_valid_mp_table_checksums() {
        let image = build(&[0, 1], 0xfee0_0000, 0xfec0_0000);
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

    #[test]
    fn pci_intx_entries_are_low_active_level_triggered() {
        let (device, _, pin, guest_gsi) = crate::boot::x86_qemu_passthrough_block_intx();
        let source_irq = (device << 2) | (pin - 1);
        let entry = interrupt_entry(
            0,
            PCI_INTX_IRQ_FLAGS,
            BUS_ID_PCI,
            source_irq,
            guest_gsi as u8,
        );

        assert_eq!(u16::from_le_bytes([entry[2], entry[3]]), 0x0f);
        assert_eq!(entry[4], BUS_ID_PCI);
        assert_eq!(entry[5], source_irq);
        assert_eq!(entry[7], guest_gsi as u8);
    }

    #[test]
    fn com1_is_identity_routed_to_ioapic_gsi4() {
        let entry = config_entries(&[0], 0xfec0_0000)
            .into_iter()
            .find(|entry| {
                entry.len() == 8 && entry[0] == 3 && entry[4] == BUS_ID_ISA && entry[5] == 4
            })
            .expect("MP table must describe the machine-owned COM1 interrupt");

        assert_eq!(
            u16::from_le_bytes([entry[2], entry[3]]),
            MP_IRQ_FLAGS_CONFORMING
        );
        assert_eq!(entry[6], IO_APIC_ID);
        assert_eq!(entry[7], 4);
    }

    #[test]
    fn q35_dev3_inta_uses_swizzled_gsi19() {
        assert_eq!(pci_intx_gsi(3, 0), 19);
    }

    #[test]
    fn io_apic_entry_uses_the_selected_machine_address() {
        let selected_address = 0xfed0_0000;
        let image = build(&[0], 0xfee0_0000, selected_address);
        let io_apic_offset = 44 + 20 + 8 + 8;
        let io_apic = &image[io_apic_offset..io_apic_offset + 8];

        assert_eq!(io_apic[0], 2);
        assert_eq!(
            u32::from_le_bytes(io_apic[4..8].try_into().unwrap()),
            selected_address
        );
    }
}
