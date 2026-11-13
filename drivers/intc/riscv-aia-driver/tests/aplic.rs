//! Public MMIO boundary contract against exclusively borrowed host memory.
//! Hardware delivery and input resampling semantics require QEMU or a board.
use core::{num::NonZeroU32, ptr::NonNull};

use riscv_aia_driver::{Aplic, Error, MsiTarget, SourceMode};

fn source(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

#[test]
fn configure_and_resample_last_source_within_window() {
    let mut window = [0u32; 0x4000 / 4];
    let base = NonNull::new(window.as_mut_ptr().cast()).unwrap();
    {
        // SAFETY: the aligned backing array is exclusively leased until the
        // capability is dropped and remains live on this test thread.
        let mut aplic = unsafe { Aplic::new(base, 0x4000, 1023) }.unwrap();
        aplic.reset_msi();
        let irq = source(1023);
        aplic.set_enabled(irq, false).unwrap();
        aplic.set_source_mode(irq, SourceMode::LevelLow).unwrap();
        aplic
            .set_target(irq, MsiTarget::new(0x3fff, irq).unwrap())
            .unwrap();
        aplic.resample(irq).unwrap();
        aplic.set_enabled(irq, true).unwrap();
        aplic.enable_msi_domain();
    }
    // These are the externally visible configuration and resampling commands,
    // including the final word at the end of the declared mapping.
    assert_eq!(window[0], (1 << 8) | (1 << 2));
    assert_eq!(window[1023], 7);
    assert_eq!(window[0x3ffc / 4], (0x3fff << 18) | 1023);
    assert_eq!(window[0x1fdc / 4], 1023);
    assert_eq!(window[0x1edc / 4], 1023);
    assert_eq!(window[0x2000 / 4], 1023);
}

#[test]
fn reject_invalid_configuration_without_register_side_effects() {
    let mut window = [0x55aa55aau32; 0x4000 / 4];
    let base = NonNull::new(window.as_mut_ptr().cast()).unwrap();
    // SAFETY: the full array remains live and exclusively available; each
    // rejected construction returns without retaining or accessing the pointer.
    unsafe {
        assert!(matches!(
            Aplic::new(base, 0x3ffc, 1),
            Err(Error::InvalidWindow)
        ));
        assert!(matches!(
            Aplic::new(base, 0x4000, 1024),
            Err(Error::InvalidSource)
        ));
        assert!(matches!(
            Aplic::new(base, 0x4000, 0),
            Err(Error::InvalidSource)
        ));
    }
    {
        // SAFETY: aligned exclusive backing storage, live throughout the lease.
        let mut aplic = unsafe { Aplic::new(base, 0x4000, 31) }.unwrap();
        let invalid = source(u32::MAX);
        let target = MsiTarget::new(0, source(1)).unwrap();
        assert_eq!(
            aplic.set_source_mode(invalid, SourceMode::EdgeRising),
            Err(Error::InvalidSource)
        );
        assert_eq!(aplic.set_target(invalid, target), Err(Error::InvalidSource));
        assert_eq!(aplic.set_enabled(invalid, true), Err(Error::InvalidSource));
        assert_eq!(aplic.resample(invalid), Err(Error::InvalidSource));
        assert_eq!(aplic.set_pending(invalid), Err(Error::InvalidSource));
        assert_eq!(
            MsiTarget::new(1 << 14, source(1)),
            Err(Error::InvalidTarget)
        );
        assert_eq!(
            MsiTarget::new(0, source(1 << 11)),
            Err(Error::InvalidTarget)
        );
    }
    assert!(window.iter().all(|word| *word == 0x55aa55aa));
}
