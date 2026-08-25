//! GICv3 non-maskable interrupt attribute programming.

use tock_registers::{interfaces::*, registers::ReadWrite};

use super::{Affinity, Gic, SecurityState, gicd::TYPER, redistributor_for_affinity_from};
use crate::{
    IntId,
    define::{NmiAttributeSlot, nmi_attribute_slot},
};

/// The non-maskable property assigned to a Group 1 interrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NmiAttribute {
    /// The interrupt follows ordinary priority masking rules.
    Maskable,
    /// The interrupt has the architectural non-maskable property.
    NonMaskable,
}

impl NmiAttribute {
    fn from_register(register: u32, mask: u32) -> Self {
        if register & mask == 0 {
            Self::Maskable
        } else {
            Self::NonMaskable
        }
    }

    fn update_register(self, register: u32, mask: u32) -> u32 {
        match self {
            Self::Maskable => register & !mask,
            Self::NonMaskable => register | mask,
        }
    }
}

/// Failure returned while accessing a GIC NMI attribute register.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NmiAttributeError {
    /// The interrupt controller does not implement `FEAT_GICv3_NMI`.
    #[error("the GIC does not implement FEAT_GICv3_NMI")]
    Unsupported,

    /// The requested INTID has no standard GICD/GICR NMI attribute slot.
    #[error("NMI attributes are not supported for {0:?}")]
    UnsupportedIntId(IntId),

    /// The requested SPI is outside the implemented Distributor range.
    #[error("{0:?} is not implemented by this GIC Distributor")]
    UnimplementedIntId(IntId),

    /// The interrupt is Group 0 or is not accessible as Group 1.
    #[error("{0:?} is not configured as an accessible Group 1 interrupt")]
    NotAccessibleGroup1(IntId),

    /// Affinity routing is disabled for the interrupt's Security state.
    #[error("affinity routing is disabled for {0:?}")]
    AffinityRoutingDisabled(IntId),

    /// The interrupt must be disabled before changing its NMI attribute.
    #[error("cannot change the NMI attribute of enabled interrupt {0:?}")]
    InterruptEnabled(IntId),

    /// The interrupt must be inactive before changing its NMI attribute.
    #[error("cannot change the NMI attribute of active interrupt {0:?}")]
    InterruptActive(IntId),

    /// No Redistributor frame matches the current PE.
    #[error("no Redistributor matches current CPU affinity {0:?}")]
    CurrentRedistributorNotFound(Affinity),
}

impl Gic {
    /// Report whether the GIC implements NMI attribute registers.
    ///
    /// `GICD_TYPER.NMI` is the architectural capability source for both
    /// `GICD_INMIR<n>` and `GICR_INMIR0`. A clear bit means those registers
    /// are RES0 and must not be probed by temporarily modifying an interrupt.
    pub fn supports_nmi_attributes(&self) -> bool {
        self.gicd().TYPER.is_set(TYPER::NMI)
    }

    /// Set the non-maskable property of a standard SGI, PPI, or SPI.
    ///
    /// Private interrupts are changed only for the current PE. The caller must
    /// initialize the Distributor before using this method and must also
    /// initialize the current Redistributor when changing a private interrupt.
    /// The interrupt must be disabled and inactive; this method checks both
    /// states before modifying the attribute. A pending interrupt is permitted
    /// and observes either the old or new attribute as required by the GIC
    /// architecture. This API programs the interrupt property only; it does not
    /// provide the NMI acknowledge or exception-handling path.
    ///
    /// The caller must serialize independent MMIO aliases and interrupt
    /// handling for the INTID until this method returns. Some GIC
    /// implementations keep SGIs permanently enabled; those SGIs cannot meet
    /// this API's quiescent-state contract and return
    /// [`NmiAttributeError::InterruptEnabled`].
    ///
    /// # Errors
    ///
    /// Returns [`NmiAttributeError`] if the capability is absent, the INTID is
    /// unsupported or unimplemented, the interrupt is not accessible Group 1,
    /// affinity routing is disabled, the current Redistributor is missing, or
    /// the interrupt is enabled or active.
    pub fn set_nmi_attribute(
        &mut self,
        intid: IntId,
        attribute: NmiAttribute,
    ) -> Result<(), NmiAttributeError> {
        self.set_nmi_attribute_with_current_affinity(intid, attribute, Affinity::current)
    }

    fn set_nmi_attribute_with_current_affinity(
        &mut self,
        intid: IntId,
        attribute: NmiAttribute,
        current_affinity: impl FnOnce() -> Affinity,
    ) -> Result<(), NmiAttributeError> {
        let registers = self.nmi_attribute_register(intid, current_affinity)?;
        registers.ensure_disabled_and_inactive(intid)?;
        registers
            .attribute
            .set(attribute.update_register(registers.attribute.get(), registers.mask));
        Ok(())
    }

    /// Read the non-maskable property of a standard SGI, PPI, or SPI.
    ///
    /// The caller must initialize the Distributor first so this method observes
    /// the Security state established by [`Gic::init`]. Private interrupts are
    /// read from the current PE's Redistributor, which must also be initialized.
    ///
    /// # Errors
    ///
    /// Returns [`NmiAttributeError`] if the capability is absent, the INTID is
    /// unsupported or unimplemented, the interrupt is not accessible Group 1,
    /// affinity routing is disabled, or the current Redistributor is missing.
    /// Reading the attribute does not require the interrupt to be disabled or
    /// inactive.
    pub fn nmi_attribute(&self, intid: IntId) -> Result<NmiAttribute, NmiAttributeError> {
        self.nmi_attribute_with_current_affinity(intid, Affinity::current)
    }

    fn nmi_attribute_with_current_affinity(
        &self,
        intid: IntId,
        current_affinity: impl FnOnce() -> Affinity,
    ) -> Result<NmiAttribute, NmiAttributeError> {
        let registers = self.nmi_attribute_register(intid, current_affinity)?;
        Ok(NmiAttribute::from_register(
            registers.attribute.get(),
            registers.mask,
        ))
    }

    fn nmi_attribute_register(
        &self,
        intid: IntId,
        current_affinity: impl FnOnce() -> Affinity,
    ) -> Result<NmiAttributeRegisters<'_>, NmiAttributeError> {
        let slot = nmi_attribute_slot(intid).ok_or(NmiAttributeError::UnsupportedIntId(intid))?;
        if !self.supports_nmi_attributes() {
            return Err(NmiAttributeError::Unsupported);
        }

        match slot {
            NmiAttributeSlot::Redistributor { mask } => {
                self.redistributor_nmi_register(intid, mask, current_affinity())
            }
            NmiAttributeSlot::Distributor { register, mask } => {
                self.distributor_nmi_register(intid, register, mask)
            }
        }
    }

    fn redistributor_nmi_register(
        &self,
        intid: IntId,
        mask: u32,
        affinity: Affinity,
    ) -> Result<NmiAttributeRegisters<'_>, NmiAttributeError> {
        let redistributor = redistributor_for_affinity_from(self.gicr, affinity)
            .ok_or(NmiAttributeError::CurrentRedistributorNotFound(affinity))?;
        // SAFETY: the pointer belongs to the Redistributor mapping whose
        // lifetime and exclusive ownership are guaranteed by `Gic::new`.
        let redistributor = unsafe { redistributor.as_ref() };
        let group = self.group1_access(
            intid,
            redistributor.sgi.IGROUPR0.get() & mask != 0,
            redistributor.sgi.IGRPMODR0.get() & mask != 0,
        )?;
        self.ensure_affinity_routing(intid, group)?;
        Ok(NmiAttributeRegisters {
            attribute: &redistributor.sgi.INMIR0,
            enabled: &redistributor.sgi.ISENABLER0,
            active: &redistributor.sgi.ISACTIVER0,
            mask,
        })
    }

    fn distributor_nmi_register(
        &self,
        intid: IntId,
        register: usize,
        mask: u32,
    ) -> Result<NmiAttributeRegisters<'_>, NmiAttributeError> {
        if intid.to_u32() >= self.gicd().max_spi_num() {
            return Err(NmiAttributeError::UnimplementedIntId(intid));
        }

        let group = self.group1_access(
            intid,
            self.gicd().IGROUPR[register].get() & mask != 0,
            self.gicd().IGRPMODR[register].get() & mask != 0,
        )?;
        self.ensure_affinity_routing(intid, group)?;
        Ok(NmiAttributeRegisters {
            attribute: &self.gicd().INMIR[register],
            enabled: &self.gicd().ISENABLER[register],
            active: &self.gicd().ISACTIVER[register],
            mask,
        })
    }

    fn group1_access(
        &self,
        intid: IntId,
        group1: bool,
        group_modifier: bool,
    ) -> Result<NmiGroup, NmiAttributeError> {
        match (self.security_state, group1, group_modifier) {
            (SecurityState::Single, true, _) => Ok(NmiGroup::Single),
            (SecurityState::Secure, true, _) => Ok(NmiGroup::NonSecure),
            (SecurityState::Secure, false, true) => Ok(NmiGroup::Secure),
            (SecurityState::NonSecure, true, _) => Ok(NmiGroup::NonSecure),
            _ => Err(NmiAttributeError::NotAccessibleGroup1(intid)),
        }
    }

    fn ensure_affinity_routing(
        &self,
        intid: IntId,
        group: NmiGroup,
    ) -> Result<(), NmiAttributeError> {
        let mask = match (self.security_state, group) {
            (SecurityState::Single, NmiGroup::Single) => 1 << 4,
            (SecurityState::Secure, NmiGroup::Secure) => 1 << 4,
            (SecurityState::Secure, NmiGroup::NonSecure) => 1 << 5,
            (SecurityState::NonSecure, NmiGroup::NonSecure) => 1 << 4,
            _ => 0,
        };
        if self.gicd().CTLR.get() & mask == 0 {
            return Err(NmiAttributeError::AffinityRoutingDisabled(intid));
        }
        Ok(())
    }
}

struct NmiAttributeRegisters<'a> {
    attribute: &'a ReadWrite<u32>,
    enabled: &'a ReadWrite<u32>,
    active: &'a ReadWrite<u32>,
    mask: u32,
}

impl NmiAttributeRegisters<'_> {
    fn ensure_disabled_and_inactive(&self, intid: IntId) -> Result<(), NmiAttributeError> {
        if self.enabled.get() & self.mask != 0 {
            return Err(NmiAttributeError::InterruptEnabled(intid));
        }
        if self.active.get() & self.mask != 0 {
            return Err(NmiAttributeError::InterruptActive(intid));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum NmiGroup {
    Single,
    Secure,
    NonSecure,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::mem::size_of;
    use std::boxed::Box;

    use super::*;
    use crate::VirtAddr;

    const GICD_SIZE: usize = 0x8000;
    const GICR_V3_SIZE: usize = 0x20000;
    const GICD_CTLR: usize = 0x0000;
    const GICD_TYPER: usize = 0x0004;
    const GICD_IGROUPR: usize = 0x0080;
    const GICD_ISENABLER: usize = 0x0100;
    const GICD_ISACTIVER: usize = 0x0300;
    const GICD_INMIR: usize = 0x0f80;
    const GICR_TYPER: usize = 0x0008;
    const GICR_SGI_BASE: usize = 0x10000;
    const GICR_IGROUPR0: usize = GICR_SGI_BASE + 0x0080;
    const GICR_ISENABLER0: usize = GICR_SGI_BASE + 0x0100;
    const GICR_ISACTIVER0: usize = GICR_SGI_BASE + 0x0300;
    const GICR_IGRPMODR0: usize = GICR_SGI_BASE + 0x0d00;
    const GICR_INMIR0: usize = GICR_SGI_BASE + 0x0f80;
    const GICR_TYPER_LAST: u64 = 1 << 4;
    const GICD_TYPER_NMI: u32 = 1 << 9;
    const GICD_CTLR_ARE_S_OR_ONE: u32 = 1 << 4;
    const GICD_CTLR_ARE_NS_SECURE_VIEW: u32 = 1 << 5;
    const TEST_AFFINITY: Affinity = Affinity {
        aff0: 4,
        aff1: 3,
        aff2: 2,
        aff3: 1,
    };

    #[derive(Clone, Copy, Debug)]
    struct PrivateInterruptConfig {
        affinity: Affinity,
        security_state: SecurityState,
        ctlr: u32,
        group1_mask: u32,
        group_modifier_mask: u32,
    }

    struct FakeGic {
        gic: Gic,
        distributor: Box<[u64]>,
        redistributor: Box<[u64]>,
    }

    impl FakeGic {
        fn new(typer: u32, ctlr: u32, group_register: usize, group_mask: u32) -> Self {
            let mut distributor = zeroed_words(GICD_SIZE);
            let base = distributor.as_mut_ptr().cast::<u8>();
            write_u32(base, GICD_TYPER, typer);
            write_u32(base, GICD_CTLR, ctlr);
            write_u32(
                base,
                GICD_IGROUPR + group_register * size_of::<u32>(),
                group_mask,
            );

            let mut redistributor = zeroed_words(GICR_V3_SIZE);
            write_u64(
                redistributor.as_mut_ptr().cast::<u8>(),
                GICR_TYPER,
                GICR_TYPER_LAST,
            );
            Self::from_registers(distributor, redistributor, SecurityState::Single)
        }

        fn with_private_interrupts(config: PrivateInterruptConfig) -> Self {
            let mut distributor = zeroed_words(GICD_SIZE);
            let distributor_base = distributor.as_mut_ptr().cast::<u8>();
            write_u32(distributor_base, GICD_TYPER, GICD_TYPER_NMI);
            write_u32(distributor_base, GICD_CTLR, config.ctlr);

            let mut redistributor = zeroed_words(GICR_V3_SIZE);
            let redistributor_base = redistributor.as_mut_ptr().cast::<u8>();
            write_u64(
                redistributor_base,
                GICR_TYPER,
                (u64::from(config.affinity.affinity()) << 32) | GICR_TYPER_LAST,
            );
            write_u32(redistributor_base, GICR_IGROUPR0, config.group1_mask);
            write_u32(
                redistributor_base,
                GICR_IGRPMODR0,
                config.group_modifier_mask,
            );

            Self::from_registers(distributor, redistributor, config.security_state)
        }

        fn from_registers(
            mut distributor: Box<[u64]>,
            mut redistributor: Box<[u64]>,
            security_state: SecurityState,
        ) -> Self {
            // SAFETY: both boxes own suitably aligned register-sized
            // allocations for the lifetime of `gic` and this fixture is their
            // sole accessor.
            let mut gic = unsafe {
                Gic::new(
                    VirtAddr::from(distributor.as_mut_ptr().cast::<u8>()),
                    VirtAddr::from(redistributor.as_mut_ptr().cast::<u8>()),
                )
            };
            gic.security_state = security_state;
            Self {
                gic,
                distributor,
                redistributor,
            }
        }

        fn read_distributor(&self, offset: usize) -> u32 {
            let address = self.distributor.as_ptr().cast::<u8>();
            // SAFETY: every caller uses an aligned u32 offset within the
            // `GICD_SIZE` allocation owned by this fixture.
            unsafe { address.add(offset).cast::<u32>().read() }
        }

        fn read_redistributor(&self, offset: usize) -> u32 {
            let address = self.redistributor.as_ptr().cast::<u8>();
            // SAFETY: every caller uses an aligned u32 offset within the
            // `GICR_V3_SIZE` allocation owned by this fixture.
            unsafe { address.add(offset).cast::<u32>().read() }
        }

        fn write_distributor(&mut self, offset: usize, value: u32) {
            write_u32(self.distributor.as_mut_ptr().cast::<u8>(), offset, value);
        }

        fn write_redistributor(&mut self, offset: usize, value: u32) {
            write_u32(self.redistributor.as_mut_ptr().cast::<u8>(), offset, value);
        }
    }

    #[test]
    fn programs_and_reads_standard_spi_nmi_attribute() {
        let intid = IntId::spi(42);
        let register = 2;
        let mask = 1 << 10;
        let mut fake = FakeGic::new(GICD_TYPER_NMI | 2, GICD_CTLR_ARE_S_OR_ONE, register, mask);

        assert!(fake.gic.supports_nmi_attributes());
        assert_eq!(
            fake.gic.nmi_attribute_with_current_affinity(intid, || {
                panic!("SPI attributes must not resolve a Redistributor")
            }),
            Ok(NmiAttribute::Maskable)
        );
        assert_eq!(fake.gic.nmi_attribute(intid), Ok(NmiAttribute::Maskable));

        fake.gic
            .set_nmi_attribute(intid, NmiAttribute::NonMaskable)
            .unwrap();
        assert_eq!(
            fake.read_distributor(GICD_INMIR + register * size_of::<u32>()),
            mask
        );
        assert_eq!(fake.gic.nmi_attribute(intid), Ok(NmiAttribute::NonMaskable));

        fake.gic
            .set_nmi_attribute(intid, NmiAttribute::Maskable)
            .unwrap();
        assert_eq!(fake.gic.nmi_attribute(intid), Ok(NmiAttribute::Maskable));
    }

    #[test]
    fn rejects_unsupported_inaccessible_and_unimplemented_spi_attributes() {
        let intid = IntId::spi(0);
        let group_register = 1;
        let group_mask = 1;

        let unsupported = FakeGic::new(1, GICD_CTLR_ARE_S_OR_ONE, group_register, group_mask);
        assert_eq!(
            unsupported.gic.nmi_attribute(intid),
            Err(NmiAttributeError::Unsupported)
        );

        let group0 = FakeGic::new(
            GICD_TYPER_NMI | 1,
            GICD_CTLR_ARE_S_OR_ONE,
            group_register,
            0,
        );
        assert_eq!(
            group0.gic.nmi_attribute(intid),
            Err(NmiAttributeError::NotAccessibleGroup1(intid))
        );

        let routing_disabled = FakeGic::new(GICD_TYPER_NMI | 1, 0, group_register, group_mask);
        assert_eq!(
            routing_disabled.gic.nmi_attribute(intid),
            Err(NmiAttributeError::AffinityRoutingDisabled(intid))
        );

        let unimplemented = FakeGic::new(
            GICD_TYPER_NMI,
            GICD_CTLR_ARE_S_OR_ONE,
            group_register,
            group_mask,
        );
        assert_eq!(
            unimplemented.gic.nmi_attribute(intid),
            Err(NmiAttributeError::UnimplementedIntId(intid))
        );

        let special = unsafe { IntId::raw(1023) };
        assert_eq!(
            unimplemented.gic.nmi_attribute(special),
            Err(NmiAttributeError::UnsupportedIntId(special))
        );
    }

    #[test]
    fn rejects_enabled_and_active_spi_attribute_changes() {
        let intid = IntId::spi(42);
        let register = 2;
        let mask = 1 << 10;

        for (state_register, expected) in [
            (GICD_ISENABLER, NmiAttributeError::InterruptEnabled(intid)),
            (GICD_ISACTIVER, NmiAttributeError::InterruptActive(intid)),
        ] {
            let mut fake = FakeGic::new(GICD_TYPER_NMI | 2, GICD_CTLR_ARE_S_OR_ONE, register, mask);
            fake.write_distributor(state_register + register * size_of::<u32>(), mask);

            assert_eq!(
                fake.gic.set_nmi_attribute(intid, NmiAttribute::NonMaskable),
                Err(expected)
            );
            assert_eq!(
                fake.read_distributor(GICD_INMIR + register * size_of::<u32>()),
                0
            );
            assert_eq!(fake.gic.nmi_attribute(intid), Ok(NmiAttribute::Maskable));
        }
    }

    #[test]
    fn programs_reads_and_clears_sgi_and_ppi_nmi_attributes() {
        let sgi = IntId::sgi(5);
        let ppi = IntId::ppi(14);
        let sgi_mask = 1 << 5;
        let ppi_mask = 1 << 30;
        let mut fake = FakeGic::with_private_interrupts(PrivateInterruptConfig {
            affinity: TEST_AFFINITY,
            security_state: SecurityState::Single,
            ctlr: GICD_CTLR_ARE_S_OR_ONE,
            group1_mask: sgi_mask | ppi_mask,
            group_modifier_mask: 0,
        });

        for intid in [sgi, ppi] {
            assert_eq!(
                fake.gic
                    .nmi_attribute_with_current_affinity(intid, || TEST_AFFINITY),
                Ok(NmiAttribute::Maskable)
            );
        }

        fake.gic
            .set_nmi_attribute_with_current_affinity(sgi, NmiAttribute::NonMaskable, || {
                TEST_AFFINITY
            })
            .unwrap();
        assert_eq!(fake.read_redistributor(GICR_INMIR0), sgi_mask);

        fake.gic
            .set_nmi_attribute_with_current_affinity(ppi, NmiAttribute::NonMaskable, || {
                TEST_AFFINITY
            })
            .unwrap();
        assert_eq!(fake.read_redistributor(GICR_INMIR0), sgi_mask | ppi_mask);

        fake.gic
            .set_nmi_attribute_with_current_affinity(sgi, NmiAttribute::Maskable, || TEST_AFFINITY)
            .unwrap();
        assert_eq!(fake.read_redistributor(GICR_INMIR0), ppi_mask);
        assert_eq!(
            fake.gic
                .nmi_attribute_with_current_affinity(sgi, || TEST_AFFINITY),
            Ok(NmiAttribute::Maskable)
        );
        assert_eq!(
            fake.gic
                .nmi_attribute_with_current_affinity(ppi, || TEST_AFFINITY),
            Ok(NmiAttribute::NonMaskable)
        );

        fake.gic
            .set_nmi_attribute_with_current_affinity(ppi, NmiAttribute::Maskable, || TEST_AFFINITY)
            .unwrap();
        assert_eq!(fake.read_redistributor(GICR_INMIR0), 0);
    }

    #[test]
    fn accepts_secure_and_nonsecure_group1_private_interrupts() {
        let cases = [
            (
                IntId::sgi(6),
                1 << 6,
                PrivateInterruptConfig {
                    affinity: TEST_AFFINITY,
                    security_state: SecurityState::Secure,
                    ctlr: GICD_CTLR_ARE_S_OR_ONE,
                    group1_mask: 0,
                    group_modifier_mask: 1 << 6,
                },
            ),
            (
                IntId::ppi(0),
                1 << 16,
                PrivateInterruptConfig {
                    affinity: TEST_AFFINITY,
                    security_state: SecurityState::Secure,
                    ctlr: GICD_CTLR_ARE_NS_SECURE_VIEW,
                    group1_mask: 1 << 16,
                    group_modifier_mask: 0,
                },
            ),
            (
                IntId::sgi(7),
                1 << 7,
                PrivateInterruptConfig {
                    affinity: TEST_AFFINITY,
                    security_state: SecurityState::NonSecure,
                    ctlr: GICD_CTLR_ARE_S_OR_ONE,
                    group1_mask: 1 << 7,
                    group_modifier_mask: 0,
                },
            ),
        ];

        for (intid, mask, config) in cases {
            let mut fake = FakeGic::with_private_interrupts(config);
            assert_eq!(
                fake.gic
                    .nmi_attribute_with_current_affinity(intid, || TEST_AFFINITY),
                Ok(NmiAttribute::Maskable),
                "{config:?}"
            );
            fake.gic
                .set_nmi_attribute_with_current_affinity(intid, NmiAttribute::NonMaskable, || {
                    TEST_AFFINITY
                })
                .unwrap();
            assert_eq!(fake.read_redistributor(GICR_INMIR0), mask, "{config:?}");
            assert_eq!(
                fake.gic
                    .nmi_attribute_with_current_affinity(intid, || TEST_AFFINITY),
                Ok(NmiAttribute::NonMaskable),
                "{config:?}"
            );
            fake.gic
                .set_nmi_attribute_with_current_affinity(intid, NmiAttribute::Maskable, || {
                    TEST_AFFINITY
                })
                .unwrap();
            assert_eq!(fake.read_redistributor(GICR_INMIR0), 0, "{config:?}");
        }
    }

    #[test]
    fn rejects_enabled_and_active_private_attribute_changes() {
        for intid in [IntId::sgi(5), IntId::ppi(14)] {
            for state_register in [GICR_ISENABLER0, GICR_ISACTIVER0] {
                let expected = match state_register {
                    GICR_ISENABLER0 => NmiAttributeError::InterruptEnabled(intid),
                    GICR_ISACTIVER0 => NmiAttributeError::InterruptActive(intid),
                    _ => unreachable!(),
                };
                assert_private_attribute_write_rejected_by_state(intid, state_register, expected);
            }
        }
    }

    #[test]
    fn rejects_inaccessible_private_groups_and_disabled_routing() {
        let intid = IntId::sgi(3);
        let mask = 1 << 3;

        assert_private_attribute_error(
            FakeGic::with_private_interrupts(PrivateInterruptConfig {
                affinity: TEST_AFFINITY,
                security_state: SecurityState::Single,
                ctlr: GICD_CTLR_ARE_S_OR_ONE,
                group1_mask: 0,
                group_modifier_mask: 0,
            }),
            intid,
            TEST_AFFINITY,
            NmiAttributeError::NotAccessibleGroup1(intid),
        );

        assert_private_attribute_error(
            FakeGic::with_private_interrupts(PrivateInterruptConfig {
                affinity: TEST_AFFINITY,
                security_state: SecurityState::NonSecure,
                ctlr: GICD_CTLR_ARE_S_OR_ONE,
                group1_mask: 0,
                group_modifier_mask: mask,
            }),
            intid,
            TEST_AFFINITY,
            NmiAttributeError::NotAccessibleGroup1(intid),
        );

        assert_private_attribute_error(
            FakeGic::with_private_interrupts(PrivateInterruptConfig {
                affinity: TEST_AFFINITY,
                security_state: SecurityState::Secure,
                ctlr: GICD_CTLR_ARE_S_OR_ONE,
                group1_mask: mask,
                group_modifier_mask: 0,
            }),
            intid,
            TEST_AFFINITY,
            NmiAttributeError::AffinityRoutingDisabled(intid),
        );
    }

    #[test]
    fn rejects_missing_current_redistributor_for_private_interrupts() {
        let intid = IntId::ppi(7);
        let mask = 1 << 23;
        let available_affinity = Affinity {
            aff0: 5,
            ..TEST_AFFINITY
        };
        let fake = FakeGic::with_private_interrupts(PrivateInterruptConfig {
            affinity: available_affinity,
            security_state: SecurityState::Single,
            ctlr: GICD_CTLR_ARE_S_OR_ONE,
            group1_mask: mask,
            group_modifier_mask: 0,
        });

        assert_private_attribute_error(
            fake,
            intid,
            TEST_AFFINITY,
            NmiAttributeError::CurrentRedistributorNotFound(TEST_AFFINITY),
        );
    }

    #[test]
    fn redistributor_lookup_compares_affinity_level_three() {
        let mut redistributor = zeroed_words(GICR_V3_SIZE);
        let base = redistributor.as_mut_ptr().cast::<u8>();
        let affinity = Affinity {
            aff0: 4,
            aff1: 3,
            aff2: 2,
            aff3: 1,
        };
        write_u64(
            base,
            GICR_TYPER,
            (u64::from(affinity.affinity()) << 32) | GICR_TYPER_LAST,
        );

        let found = redistributor_for_affinity_from(VirtAddr::from(base), affinity).unwrap();
        assert_eq!(found.as_ptr().cast::<u8>(), base);
        assert!(
            redistributor_for_affinity_from(
                VirtAddr::from(base),
                Affinity {
                    aff3: 0,
                    ..affinity
                },
            )
            .is_none()
        );
    }

    fn assert_private_attribute_error(
        mut fake: FakeGic,
        intid: IntId,
        affinity: Affinity,
        expected: NmiAttributeError,
    ) {
        assert_eq!(
            fake.gic
                .nmi_attribute_with_current_affinity(intid, || affinity),
            Err(expected)
        );
        assert_eq!(
            fake.gic.set_nmi_attribute_with_current_affinity(
                intid,
                NmiAttribute::NonMaskable,
                || affinity,
            ),
            Err(expected)
        );
    }

    fn assert_private_attribute_write_rejected_by_state(
        intid: IntId,
        state_register: usize,
        expected: NmiAttributeError,
    ) {
        let mask = 1 << (intid.to_u32() % 32);
        let mut fake = FakeGic::with_private_interrupts(PrivateInterruptConfig {
            affinity: TEST_AFFINITY,
            security_state: SecurityState::Single,
            ctlr: GICD_CTLR_ARE_S_OR_ONE,
            group1_mask: mask,
            group_modifier_mask: 0,
        });
        fake.write_redistributor(state_register, mask);

        assert_eq!(
            fake.gic.set_nmi_attribute_with_current_affinity(
                intid,
                NmiAttribute::NonMaskable,
                || TEST_AFFINITY,
            ),
            Err(expected)
        );
        assert_eq!(fake.read_redistributor(GICR_INMIR0), 0);
        assert_eq!(
            fake.gic
                .nmi_attribute_with_current_affinity(intid, || TEST_AFFINITY),
            Ok(NmiAttribute::Maskable)
        );
    }

    fn zeroed_words(size: usize) -> Box<[u64]> {
        std::vec![0; size / size_of::<u64>()].into_boxed_slice()
    }

    fn write_u32(base: *mut u8, offset: usize, value: u32) {
        // SAFETY: fixtures pass aligned offsets within their owned allocation.
        unsafe { base.add(offset).cast::<u32>().write(value) };
    }

    fn write_u64(base: *mut u8, offset: usize, value: u64) {
        // SAFETY: fixtures pass aligned offsets within their owned allocation.
        unsafe { base.add(offset).cast::<u64>().write(value) };
    }
}
