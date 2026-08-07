//! Standard x86 ACPI tables and the direct-boot low-memory image.

use std::vec::Vec;

use acpi_tables::{
    Aml,
    facs::FACS,
    fadt::{FADT, FADTBuilder, Flags as FadtFlag, PmProfile},
    gas::{AccessSize as GasAccessSize, AddressSpace as GasAddressSpace, GAS},
    madt::{EnabledStatus, IoApic, LocalInterruptController, MADT, ProcessorLocalApic},
    rsdp::Rsdp,
    xsdt::XSDT,
};

use super::{
    aml::{build_dsdt, build_spcr, oem_id, oem_revision, oem_table_id},
    config::{X86AcpiIoRegisterPlan, X86FirmwarePlan},
};
use crate::boot::acpi::*;

pub(super) const DIRECT_ACPI_BASE: u64 = 0x000e_0000;
const DIRECT_ACPI_LIMIT: u64 = 0x0010_0000;

pub(crate) fn build_direct_image(plan: &X86FirmwarePlan) -> Result<AcpiImage, AcpiBuildError> {
    let dsdt = build_dsdt(plan)?;
    let madt = build_madt(plan);
    let spcr = build_spcr(plan);
    let mut arena = AcpiTableArena::new(DIRECT_ACPI_BASE, DIRECT_ACPI_LIMIT)?;

    let rsdp_slot = arena.reserve("RSDP", Rsdp::len(), 16)?;
    let xsdt_slot = arena.reserve("XSDT", 36 + 3 * 8, 8)?;
    let fadt_slot = arena.reserve("FADT", FADT::len(), 8)?;
    let facs_slot = arena.reserve("FACS", FACS::len(), 64)?;
    let dsdt_slot = arena.reserve("DSDT", dsdt.len(), 8)?;
    let madt_slot = arena.reserve("MADT", madt.len(), 8)?;
    let spcr_slot = arena.reserve("SPCR", spcr.len(), 8)?;

    let fadt = build_fadt(plan, dsdt_slot.gpa(), facs_slot.gpa());
    let mut xsdt = XSDT::new(oem_id(), oem_table_id(), oem_revision());
    xsdt.add_entry(fadt_slot.gpa());
    xsdt.add_entry(madt_slot.gpa());
    xsdt.add_entry(spcr_slot.gpa());
    let xsdt = serialize(&xsdt);
    let rsdp = serialize(&Rsdp::new(oem_id(), xsdt_slot.gpa()));
    let facs = serialize(&FACS::new());

    arena.write(&rsdp_slot, &rsdp)?;
    arena.write(&xsdt_slot, &xsdt)?;
    arena.write(&fadt_slot, &fadt)?;
    arena.write(&facs_slot, &facs)?;
    arena.write(&dsdt_slot, &dsdt)?;
    arena.write(&madt_slot, &madt)?;
    arena.write(&spcr_slot, &spcr)?;

    let mut set = AcpiTableSet::new();
    set.add(AcpiTableRecord::new(*b"XSDT", xsdt_slot.gpa(), xsdt.len()))?;
    set.add(AcpiTableRecord::new(*b"FACP", fadt_slot.gpa(), fadt.len()))?;
    set.add(AcpiTableRecord::new(*b"FACS", facs_slot.gpa(), facs.len()))?;
    set.add(AcpiTableRecord::new(*b"DSDT", dsdt_slot.gpa(), dsdt.len()))?;
    set.add(AcpiTableRecord::new(*b"APIC", madt_slot.gpa(), madt.len()))?;
    set.add(AcpiTableRecord::new(*b"SPCR", spcr_slot.gpa(), spcr.len()))?;
    Ok(AcpiImage::new(
        DIRECT_ACPI_BASE,
        rsdp_slot.gpa(),
        arena.into_bytes(),
        set,
    ))
}

pub(super) fn build_fadt(plan: &X86FirmwarePlan, dsdt_address: u64, facs_address: u64) -> Vec<u8> {
    let mut builder = FADTBuilder::new(oem_id(), oem_table_id(), oem_revision())
        .dsdt_64(dsdt_address)
        .firmware_ctrl_64(facs_address)
        .preferred_pm_profile(PmProfile::Unspecified)
        .flag(FadtFlag::Headless);
    builder.sci_int = plan.power.sci_irq.into();
    builder.pm1a_evt_blk = u32::from(plan.power.pm1_event.port).into();
    builder.pm1_evt_len = plan.power.pm1_event.length;
    builder.x_pm1a_evt_blk = system_io_gas(plan.power.pm1_event, GasAccessSize::DwordAccess);
    builder.pm1a_cnt_blk = u32::from(plan.power.pm1_control.port).into();
    builder.pm1_cnt_len = plan.power.pm1_control.length;
    builder.x_pm1a_cnt_blk = system_io_gas(plan.power.pm1_control, GasAccessSize::WordAccess);
    builder.pm_tmr_blk = u32::from(plan.power.pm_timer.port).into();
    builder.pm_tmr_len = plan.power.pm_timer.length;
    builder.x_pm_tmr_blk = system_io_gas(plan.power.pm_timer, GasAccessSize::DwordAccess);
    serialize(&builder.finalize())
}

fn system_io_gas(register: X86AcpiIoRegisterPlan, access_size: GasAccessSize) -> GAS {
    GAS::new(
        GasAddressSpace::SystemIo,
        register.length * 8,
        0,
        access_size,
        u64::from(register.port),
    )
}

pub(super) fn build_madt(plan: &X86FirmwarePlan) -> Vec<u8> {
    let mut madt = MADT::new(
        oem_id(),
        oem_table_id(),
        oem_revision(),
        LocalInterruptController::Address(plan.interrupts.local_apic_base),
    );
    for (uid, apic_id) in plan.cpus.apic_ids.iter().copied().enumerate() {
        madt.add_structure(ProcessorLocalApic::new(
            uid as u8,
            apic_id,
            EnabledStatus::Enabled,
        ));
    }
    madt.add_structure(IoApic::new(
        plan.interrupts.io_apic_id,
        plan.interrupts.io_apic_base,
        plan.interrupts.gsi_base,
    ));
    serialize(&madt)
}

pub(super) fn serialize(table: &dyn Aml) -> Vec<u8> {
    let mut bytes = Vec::new();
    table.to_aml_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_image_has_valid_pointer_closure_and_checksums() {
        let image = build_direct_image(&super::super::config::test_plan(4)).unwrap();
        assert_eq!(image.rsdp_gpa() % 16, 0);
        assert_eq!(&image.bytes()[..8], b"RSD PTR ");
        assert_eq!(checksum(&image.bytes()[..20]), 0);
        assert_eq!(checksum(&image.bytes()[..36]), 0);

        let xsdt_gpa = u64::from_le_bytes(image.bytes()[24..32].try_into().unwrap());
        let xsdt = image.tables().find(*b"XSDT").unwrap();
        assert_eq!(xsdt.address(), xsdt_gpa);
        for table in image.tables().iter() {
            let offset = (table.address() - image.load_gpa()) as usize;
            let bytes = &image.bytes()[offset..offset + table.length()];
            assert_eq!(&bytes[..4], &table.signature());
            if table.signature() != *b"FACS" {
                assert_eq!(checksum(bytes), 0, "{:?}", table.signature());
            }
        }

        let fadt = image.tables().find(*b"FACP").unwrap();
        let offset = (fadt.address() - image.load_gpa()) as usize;
        let flags = u32::from_le_bytes(
            image.bytes()[offset + 112..offset + 116]
                .try_into()
                .unwrap(),
        );
        assert_eq!(flags & FadtFlag::HwReducedAcpi as u32, 0);
        assert_eq!(
            u16::from_le_bytes(image.bytes()[offset + 46..offset + 48].try_into().unwrap()),
            9
        );
        assert_eq!(
            u32::from_le_bytes(image.bytes()[offset + 56..offset + 60].try_into().unwrap()),
            0x600
        );
        assert_eq!(
            u32::from_le_bytes(image.bytes()[offset + 64..offset + 68].try_into().unwrap()),
            0x604
        );
        assert_eq!(
            u32::from_le_bytes(image.bytes()[offset + 76..offset + 80].try_into().unwrap()),
            0x608
        );
        assert_eq!(image.bytes()[offset + 88], 4);
        assert_eq!(image.bytes()[offset + 89], 2);
        assert_eq!(image.bytes()[offset + 91], 4);

        let extended_timer = &image.bytes()[offset + 208..offset + 220];
        assert_eq!(&extended_timer[..4], &[1, 32, 0, 3]);
        assert_eq!(
            u64::from_le_bytes(extended_timer[4..].try_into().unwrap()),
            0x608
        );
    }

    fn checksum(bytes: &[u8]) -> u8 {
        bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))
    }
}
