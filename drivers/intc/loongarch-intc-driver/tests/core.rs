mod common;

use common::{FakeIocsr, IocsrWrite, test_mmio};
use loongarch_intc_driver::{
    CpuIrqLine, EioIntcConfig, EioIntcParts, EioVector, IntcError, LioInput, LioIntcConfig,
    LioIntcParts, PchInput, PchIrqPolarity, PchIrqTrigger, PchPicConfig, PchPicParts,
};

const EIO_MISC: usize = 0x420;
const EIO_NODEMAP: usize = 0x14a0;
const EIO_IPMAP: usize = 0x14c0;
const EIO_ENABLE: usize = 0x1600;
const EIO_BOUNCE: usize = 0x1680;
const EIO_ISR: usize = 0x1800;
const EIO_ROUTE: usize = 0x1c00;

const PCH_MASK: usize = 0x20;
const PCH_EDGE: usize = 0x60;
const PCH_HTVEC: usize = 0x200;
const PCH_POLARITY: usize = 0x3e0;

const LIO_ENABLE: usize = 0x28;
const LIO_DISABLE: usize = 0x2c;
const LIO_POLARITY: usize = 0x30;
const LIO_EDGE: usize = 0x34;

#[test]
fn eio_initializes_cpu0_routes_and_handles_pending_w1_completion() {
    let iocsr = FakeIocsr::new();
    iocsr.set_u64(EIO_MISC, 0x40);
    let mut parts = EioIntcParts::new(iocsr.clone(), EioIntcConfig::new(256).unwrap()).unwrap();

    assert_eq!(iocsr.read_u64(EIO_MISC), 0x40 | (1u64 << 48));
    assert_eq!(iocsr.read_u32(EIO_NODEMAP), 0x0002_0001);
    assert_eq!(iocsr.read_u32(EIO_NODEMAP + 7 * 4), 0x8000_4000);
    assert_eq!(iocsr.read_u32(EIO_IPMAP), 0x0202_0202);
    assert_eq!(iocsr.read_u32(EIO_IPMAP + 4), 0x0202_0202);
    assert_eq!(iocsr.read_u32(EIO_ROUTE + 63 * 4), 0x0101_0101);
    assert_eq!(iocsr.read_u64(EIO_BOUNCE + 8), u64::MAX);

    let vector = EioVector::new(65).unwrap();
    parts.controller.set_enabled(vector, true).unwrap();
    assert_eq!(iocsr.read_u64(EIO_ENABLE + 8), 1 << 1);
    assert!(iocsr.writes().contains(&IocsrWrite::U64 {
        offset: EIO_BOUNCE + 8,
        value: u64::MAX,
    }));

    iocsr.set_u64(EIO_ISR + 8, 1 << 1);
    assert_eq!(parts.cpu_interface.claim(), Some(vector));
    parts.cpu_interface.complete(vector).unwrap();
    assert_eq!(
        iocsr.writes().last(),
        Some(&IocsrWrite::U64 {
            offset: EIO_ISR + 8,
            value: 1 << 1,
        })
    );

    parts.controller.set_enabled(vector, false).unwrap();
    assert_eq!(iocsr.read_u64(EIO_ENABLE + 8), 0);
}

#[test]
fn eio_rejects_invalid_counts_and_instance_out_of_range_vectors() {
    assert!(matches!(
        EioIntcConfig::new(0),
        Err(IntcError::InvalidCount { .. })
    ));
    assert!(matches!(
        EioIntcConfig::new(192),
        Err(IntcError::InvalidCountGranularity { .. })
    ));
    assert_eq!(EioVector::new(256), Err(IntcError::InvalidEioVector(256)));

    let iocsr = FakeIocsr::new();
    let mut parts = EioIntcParts::new(iocsr, EioIntcConfig::new(128).unwrap()).unwrap();
    assert!(matches!(
        parts
            .controller
            .set_enabled(EioVector::new(200).unwrap(), true),
        Err(IntcError::OutsideConfiguredRange { .. })
    ));
}

#[test]
fn pch_detects_identity_configures_input_and_maps_external_vector() {
    let mut backing = [0u64; 128];
    let mmio = test_mmio(0x1000_0000, &mut backing);
    mmio.write(0, 63u64 << 48);
    mmio.write(PCH_MASK + 4, u32::MAX);
    mmio.write(PCH_EDGE, u32::MAX);
    mmio.write(PCH_EDGE + 4, u32::MAX);
    mmio.write(PCH_POLARITY, u32::MAX);
    mmio.write(PCH_POLARITY + 4, u32::MAX);

    let config = PchPicConfig::detect(&mmio, 64, 7).unwrap();
    assert_eq!(config.input_count(), 64);
    let mut parts = PchPicParts::new(mmio.clone(), config).unwrap();
    assert_eq!(mmio.read::<u32>(PCH_EDGE), 0);
    assert_eq!(mmio.read::<u32>(PCH_EDGE + 4), 0);
    assert_eq!(mmio.read::<u32>(PCH_POLARITY), 0);
    assert_eq!(mmio.read::<u32>(PCH_POLARITY + 4), 0);

    let input = PchInput::new(33).unwrap();
    parts
        .controller
        .configure_input(input, PchIrqTrigger::Edge, PchIrqPolarity::ActiveLow)
        .unwrap();
    assert_eq!(mmio.read::<u32>(PCH_EDGE + 4), 1 << 1);
    assert_eq!(mmio.read::<u32>(PCH_POLARITY + 4), 1 << 1);
    assert_eq!(mmio.read::<u8>(PCH_HTVEC + 33), 97);

    parts.controller.set_enabled(input, true).unwrap();
    assert_eq!(mmio.read::<u32>(PCH_MASK + 4), !(1 << 1));
    assert_eq!(
        parts
            .cpu_interface
            .input_for_external_vector(EioVector::new(97).unwrap()),
        Some(input)
    );
    assert_eq!(
        parts
            .cpu_interface
            .external_vector_for_input(input)
            .unwrap(),
        EioVector::new(97).unwrap()
    );

    parts.controller.set_enabled(input, false).unwrap();
    assert_eq!(mmio.read::<u32>(PCH_MASK + 4), u32::MAX);
}

#[test]
fn pch_rejects_small_mmio_and_invalid_vector_ranges() {
    assert!(matches!(
        PchPicConfig::new(250, 64, 0),
        Err(IntcError::InvalidPchVectorRange { .. })
    ));
    assert_eq!(PchInput::new(64), Err(IntcError::InvalidPchInput(64)));

    let mut tiny = [0u64; 1];
    let mmio = test_mmio(0, &mut tiny);
    let error = PchPicParts::new(mmio, PchPicConfig::new(0, 1, 0).unwrap()).unwrap_err();
    assert!(matches!(error, IntcError::MmioTooSmall { .. }));
}

#[test]
fn lio_initializes_routes_and_shares_only_enabled_snapshot_with_cpu_interface() {
    let mut registers = [0u64; 8];
    let mut isr_backing = [0u32; 1];
    let regs = test_mmio(0x1fe0_1400, &mut registers);
    let isr = test_mmio(0x1fe0_1040, &mut isr_backing);
    let line2 = CpuIrqLine::new(2).unwrap();
    let line3 = CpuIrqLine::new(3).unwrap();
    let config = LioIntcConfig::new(
        [Some(line2), Some(line3), None, None],
        [!(1 << 5), 1 << 5, 0, 0],
    )
    .unwrap();
    let mut parts = LioIntcParts::new(regs.clone(), isr.clone(), config).unwrap();

    assert_eq!(regs.read::<u8>(0), 0x11);
    assert_eq!(regs.read::<u8>(5), 0x21);
    assert_eq!(regs.read::<u32>(LIO_DISABLE), u32::MAX);
    assert_eq!(regs.read::<u32>(LIO_EDGE), 0);
    assert_eq!(regs.read::<u32>(LIO_POLARITY), 0);

    let input = LioInput::new(5).unwrap();
    isr.write(0, 1u32 << 5);
    assert_eq!(parts.cpu_interface.claim(line3), None);

    parts.controller.set_enabled(input, true);
    assert_eq!(regs.read::<u32>(LIO_ENABLE), 1 << 5);
    assert_eq!(parts.cpu_interface.claim(line3), Some(input));
    assert_eq!(parts.cpu_interface.claim(CpuIrqLine::new(4).unwrap()), None);

    let registers_before_complete = registers;
    parts.cpu_interface.complete(input);
    assert_eq!(registers, registers_before_complete);

    parts.controller.set_enabled(input, false);
    assert_eq!(regs.read::<u32>(LIO_DISABLE), 1 << 5);
    assert_eq!(parts.cpu_interface.claim(line3), None);
}

#[test]
fn lio_claims_only_inputs_routed_to_the_triggering_parent() {
    let mut registers = [0u64; 8];
    let mut isr_backing = [0u32; 1];
    let regs = test_mmio(0x1fe0_1400, &mut registers);
    let isr = test_mmio(0x1fe0_1040, &mut isr_backing);
    let line2 = CpuIrqLine::new(2).unwrap();
    let line3 = CpuIrqLine::new(3).unwrap();
    let config = LioIntcConfig::new(
        [Some(line2), Some(line3), None, None],
        [1 << 5, 1 << 6, 0, 0],
    )
    .unwrap();
    let mut parts = LioIntcParts::new(regs, isr.clone(), config).unwrap();
    let input5 = LioInput::new(5).unwrap();
    let input6 = LioInput::new(6).unwrap();
    let fallback_input = LioInput::new(7).unwrap();

    parts.controller.set_enabled(input5, true);
    parts.controller.set_enabled(input6, true);
    parts.controller.set_enabled(fallback_input, true);

    isr.write(0, (1u32 << 5) | (1u32 << 6));
    assert_eq!(parts.cpu_interface.claim(line2), Some(input5));
    assert_eq!(parts.cpu_interface.claim(line3), Some(input6));

    isr.write(0, 1u32 << 6);
    assert_eq!(parts.cpu_interface.claim(line2), None);
    assert_eq!(parts.cpu_interface.claim(line3), Some(input6));

    isr.write(0, 1u32 << fallback_input.raw());
    assert_eq!(parts.cpu_interface.claim(line2), Some(fallback_input));
    assert_eq!(parts.cpu_interface.claim(line3), None);
}

#[test]
fn lio_rejects_invalid_parent_configuration_and_small_mappings() {
    assert_eq!(
        LioIntcConfig::new([None; 4], [0; 4]),
        Err(IntcError::MissingLioParent)
    );
    assert!(matches!(
        LioIntcConfig::new(
            [Some(CpuIrqLine::new(3).unwrap()), None, None, None],
            [0; 4]
        ),
        Err(IntcError::InvalidLioParentSlot { .. })
    ));
    assert!(matches!(
        LioIntcConfig::new(
            [Some(CpuIrqLine::new(2).unwrap()), None, None, None],
            [0, 1, 0, 0]
        ),
        Err(IntcError::LioMapWithoutParent { .. })
    ));
    assert_eq!(LioInput::new(32), Err(IntcError::InvalidLioInput(32)));

    let mut tiny_regs = [0u32; 1];
    let mut isr_backing = [0u32; 1];
    let config = LioIntcConfig::new(
        [Some(CpuIrqLine::new(2).unwrap()), None, None, None],
        [u32::MAX, 0, 0, 0],
    )
    .unwrap();
    let error = LioIntcParts::new(
        test_mmio(0, &mut tiny_regs),
        test_mmio(0, &mut isr_backing),
        config,
    )
    .unwrap_err();
    assert!(matches!(error, IntcError::MmioTooSmall { .. }));
}

#[test]
fn constructors_reject_misaligned_mmio_before_register_access() {
    let config = LioIntcConfig::new(
        [Some(CpuIrqLine::new(2).unwrap()), None, None, None],
        [u32::MAX, 0, 0, 0],
    )
    .unwrap();
    let mut lio_registers = [0u32; 14];
    let mut lio_isr = [0u32; 2];
    let error = LioIntcParts::new(
        test_mmio(0, &mut lio_registers),
        test_mmio_with_offset(0, &mut lio_isr, 1),
        config,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        IntcError::MmioMisaligned {
            region: "LIOINTC ISR",
            ..
        }
    ));

    let mut pch_identity = [0u64; 2];
    let error =
        PchPicConfig::detect(&test_mmio_with_offset(0, &mut pch_identity, 1), 0, 0).unwrap_err();
    assert!(matches!(
        error,
        IntcError::MmioMisaligned {
            region: "PCH-PIC identity",
            ..
        }
    ));

    let mut pch_registers = [0u64; 126];
    let error = PchPicParts::new(
        test_mmio_with_offset(0, &mut pch_registers, 1),
        PchPicConfig::new(0, 1, 0).unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        IntcError::MmioMisaligned {
            region: "PCH-PIC",
            ..
        }
    ));

    let mut lio_registers = [0u32; 15];
    let mut lio_isr = [0u32; 1];
    let error = LioIntcParts::new(
        test_mmio_with_offset(0, &mut lio_registers, 1),
        test_mmio(0, &mut lio_isr),
        config,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        IntcError::MmioMisaligned {
            region: "LIOINTC register",
            ..
        }
    ));
}

fn test_mmio_with_offset<T>(phys: usize, backing: &mut [T], offset: usize) -> mmio_api::MmioRaw {
    let size = core::mem::size_of_val(backing);
    assert!(offset < size);
    // SAFETY: `offset` remains inside `backing`, and the returned region ends
    // at the same allocation boundary as the original slice.
    let pointer = unsafe { backing.as_mut_ptr().cast::<u8>().add(offset) };
    let pointer = std::ptr::NonNull::new(pointer).unwrap();
    // SAFETY: tests keep `backing` alive and do not resize it while the
    // returned mapping is used; `size - offset` covers the remaining bytes.
    unsafe { mmio_api::MmioRaw::new(mmio_api::MmioAddr::from(phys), pointer, size - offset) }
}
