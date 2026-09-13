//! Compile-checked GICv3.3 NMI attribute setup for one SPI.

#[cfg(target_arch = "aarch64")]
use arm_gic_driver::{
    IntId, VirtAddr,
    v3::{Gic, NmiAttribute, NmiAttributeError},
};

#[cfg(target_arch = "aarch64")]
fn initialize_gic_with_spi_nmi(
    gicd: VirtAddr,
    gicr: VirtAddr,
    spi_number: u32,
) -> Result<(Gic, NmiAttribute), NmiAttributeError> {
    // SAFETY: OS or platform glue must keep both mapped register regions valid
    // and give this `Gic` exclusive ownership of them for its lifetime.
    let mut gic = unsafe { Gic::new(gicd, gicr) };
    gic.init();

    if !gic.supports_nmi_attributes() {
        return Err(NmiAttributeError::Unsupported);
    }

    let spi = IntId::spi(spi_number);
    gic.set_irq_enable(spi, false);
    gic.set_nmi_attribute(spi, NmiAttribute::NonMaskable)?;
    let attribute = gic.nmi_attribute(spi)?;
    Ok((gic, attribute))
}

#[cfg(target_arch = "aarch64")]
fn main() {
    // Keep the hardware-specific function compile-checked without inventing
    // board addresses or touching MMIO on the build host.
    let _initialize = initialize_gic_with_spi_nmi;
}

#[cfg(not(target_arch = "aarch64"))]
fn main() {}
