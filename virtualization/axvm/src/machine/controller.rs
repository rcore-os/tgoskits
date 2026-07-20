//! Typed interrupt-controller resources selected during machine planning.

use alloc::string::String;

use axvm_types::VmMachineMode;

use super::{
    AddressRange, HostPlatformSnapshot, MachinePlanError, MachinePlanResult, VmMachineRequest,
};

/// Architecture profile for one AArch64 GICv3 instance.
#[derive(Clone, Debug)]
pub struct Aarch64GicV3Profile {
    distributor: AddressRange,
    redistributor_base: u64,
    redistributor_stride: u64,
    its: Option<AddressRange>,
    spi_count: u32,
}

impl Aarch64GicV3Profile {
    /// Creates a checked virtual GICv3 layout template.
    pub fn new(
        distributor: AddressRange,
        redistributor_base: u64,
        redistributor_stride: u64,
        its: Option<AddressRange>,
        spi_count: u32,
    ) -> MachinePlanResult<Self> {
        if redistributor_stride == 0
            || !redistributor_stride.is_power_of_two()
            || !redistributor_base.is_multiple_of(redistributor_stride)
        {
            return Err(MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "invalid GICv3 Redistributor base {redistributor_base:#x} or stride \
                     {redistributor_stride:#x}"
                ),
            });
        }
        if spi_count == 0 || spi_count > 988 {
            return Err(MachinePlanError::InvalidFirmware {
                detail: alloc::format!("invalid GICv3 SPI count {spi_count}"),
            });
        }
        Ok(Self {
            distributor,
            redistributor_base,
            redistributor_stride,
            its,
            spi_count,
        })
    }
}

/// Architecture profile for one RISC-V PLIC instance.
#[derive(Clone, Debug)]
pub struct RiscvPlicProfile {
    mmio: AddressRange,
    source_count: u32,
}

impl RiscvPlicProfile {
    /// Creates a PLIC profile from its complete guest MMIO aperture.
    pub fn new(mmio: AddressRange, source_count: u32) -> MachinePlanResult<Self> {
        if source_count == 0 || source_count > 1023 {
            return Err(MachinePlanError::InvalidFirmware {
                detail: alloc::format!("invalid PLIC source count {source_count}"),
            });
        }
        Ok(Self { mmio, source_count })
    }
}

/// Architecture profile for an x86 local-APIC/IOAPIC topology.
#[derive(Clone, Debug)]
pub struct X86ApicProfile {
    lapic: AddressRange,
    ioapic: AddressRange,
}

impl X86ApicProfile {
    /// Creates an APIC profile from the local-APIC and IOAPIC apertures.
    pub const fn new(lapic: AddressRange, ioapic: AddressRange) -> Self {
        Self { lapic, ioapic }
    }
}

/// Architecture profile for a LoongArch PCH-PIC/EIOINTC topology.
#[derive(Clone, Debug)]
pub struct LoongArchInterruptProfile {
    pch_pic: AddressRange,
    pch_msi: AddressRange,
    routing: LoongArchInterruptRouting,
}

impl LoongArchInterruptProfile {
    /// Creates a profile from physical interrupt-controller resources.
    pub const fn new(
        pch_pic: AddressRange,
        pch_msi: AddressRange,
        routing: LoongArchInterruptRouting,
    ) -> Self {
        Self {
            pch_pic,
            pch_msi,
            routing,
        }
    }
}

/// Fixed LoongArch interrupt routing shared by ACPI and runtime topology.
#[derive(Clone, Copy, Debug)]
pub struct LoongArchInterruptRouting {
    eiointc_irq: u8,
    pch_pic_vector_base: u32,
    pch_msi_vector_base: u32,
    pch_msi_vector_count: u32,
    acpi: LoongArchAcpiInterruptRouting,
}

impl LoongArchInterruptRouting {
    /// Creates fixed controller routing metadata.
    pub const fn new(
        eiointc_irq: u8,
        pch_pic_vector_base: u32,
        pch_msi_vector_base: u32,
        pch_msi_vector_count: u32,
        acpi: LoongArchAcpiInterruptRouting,
    ) -> Self {
        Self {
            eiointc_irq,
            pch_pic_vector_base,
            pch_msi_vector_base,
            pch_msi_vector_count,
            acpi,
        }
    }

    /// Returns the CPU-local IRQ used by EIOINTC.
    pub const fn eiointc_irq(self) -> u8 {
        self.eiointc_irq
    }

    /// Returns the EIOINTC vector base of PCH-PIC inputs.
    pub const fn pch_pic_vector_base(self) -> u32 {
        self.pch_pic_vector_base
    }

    /// Returns the first EIOINTC vector owned by PCH-MSI.
    pub const fn pch_msi_vector_base(self) -> u32 {
        self.pch_msi_vector_base
    }

    /// Returns the number of controller-visible PCH-MSI vectors.
    pub const fn pch_msi_vector_count(self) -> u32 {
        self.pch_msi_vector_count
    }

    /// Returns the ACPI GSI mapping for PCH controllers.
    pub const fn acpi(self) -> LoongArchAcpiInterruptRouting {
        self.acpi
    }
}

/// ACPI GSI numbering for LoongArch PCH controllers.
#[derive(Clone, Copy, Debug)]
pub struct LoongArchAcpiInterruptRouting {
    pch_pic_gsi_base: u16,
    pch_msi_start: u32,
    pch_msi_count: u32,
}

impl LoongArchAcpiInterruptRouting {
    /// Creates ACPI GSI ranges for PCH-PIC and PCH-MSI.
    pub const fn new(pch_pic_gsi_base: u16, pch_msi_start: u32, pch_msi_count: u32) -> Self {
        Self {
            pch_pic_gsi_base,
            pch_msi_start,
            pch_msi_count,
        }
    }

    /// Returns the ACPI GSI base of PCH-PIC inputs.
    pub const fn pch_pic_gsi_base(self) -> u16 {
        self.pch_pic_gsi_base
    }

    /// Returns the first GSI owned by PCH-MSI.
    pub const fn pch_msi_start(self) -> u32 {
        self.pch_msi_start
    }

    /// Returns the number of ACPI PCH-MSI vectors.
    pub const fn pch_msi_count(self) -> u32 {
        self.pch_msi_count
    }
}

/// Controller family fixed by an architecture's standard machine profile.
#[derive(Clone, Debug)]
pub enum InterruptControllerProfile {
    /// Arm GICv3 Distributor, Redistributors, CPU interface, and optional ITS.
    Aarch64GicV3(Aarch64GicV3Profile),
    /// RISC-V PLIC with one supervisor context per vCPU.
    RiscvPlic(RiscvPlicProfile),
    /// x86 IOAPIC feeding per-vCPU local APICs.
    X86Apic(X86ApicProfile),
    /// LoongArch PCH-PIC feeding EIOINTC and vCPUs.
    LoongArch(LoongArchInterruptProfile),
}

/// Final AArch64 GICv3 resources consumed by runtime and firmware generation.
#[derive(Clone, Debug)]
pub struct Aarch64GicV3Plan {
    distributor: AddressRange,
    redistributors: AddressRange,
    redistributor_stride: u64,
    its: Option<AddressRange>,
    spi_count: u32,
}

impl Aarch64GicV3Plan {
    /// Returns the Distributor MMIO range.
    pub const fn distributor(&self) -> AddressRange {
        self.distributor
    }

    /// Returns the complete per-VM Redistributor MMIO range.
    pub const fn redistributors(&self) -> AddressRange {
        self.redistributors
    }

    /// Returns the distance between Redistributor frames.
    pub const fn redistributor_stride(&self) -> u64 {
        self.redistributor_stride
    }

    /// Returns the software ITS aperture, if exposed.
    pub const fn its(&self) -> Option<AddressRange> {
        self.its
    }

    /// Returns the number of implemented SPIs.
    pub const fn spi_count(&self) -> u32 {
        self.spi_count
    }
}

/// Final RISC-V PLIC resources.
#[derive(Clone, Debug)]
pub struct RiscvPlicPlan {
    mmio: AddressRange,
    context_count: usize,
    source_count: u32,
}

impl RiscvPlicPlan {
    /// Returns the PLIC MMIO aperture.
    pub const fn mmio(&self) -> AddressRange {
        self.mmio
    }

    /// Returns the number of guest PLIC contexts.
    pub const fn context_count(&self) -> usize {
        self.context_count
    }

    /// Returns the number of implemented external interrupt sources.
    pub const fn source_count(&self) -> u32 {
        self.source_count
    }
}

/// Final x86 APIC resources.
#[derive(Clone, Debug)]
pub struct X86ApicPlan {
    lapic: AddressRange,
    ioapic: AddressRange,
}

impl X86ApicPlan {
    /// Returns the local-APIC MMIO aperture advertised to the guest.
    pub const fn lapic(&self) -> AddressRange {
        self.lapic
    }

    /// Returns the IOAPIC MMIO aperture.
    pub const fn ioapic(&self) -> AddressRange {
        self.ioapic
    }
}

/// Final LoongArch interrupt-controller resources.
#[derive(Clone, Debug)]
pub struct LoongArchInterruptPlan {
    pch_pic: AddressRange,
    pch_msi: AddressRange,
    routing: LoongArchInterruptRouting,
}

impl LoongArchInterruptPlan {
    /// Returns the PCH-PIC MMIO aperture.
    pub const fn pch_pic(&self) -> AddressRange {
        self.pch_pic
    }

    /// Returns the PCH-MSI MMIO aperture.
    pub const fn pch_msi(&self) -> AddressRange {
        self.pch_msi
    }

    /// Returns fixed EIOINTC/PCH routing metadata.
    pub const fn routing(&self) -> LoongArchInterruptRouting {
        self.routing
    }
}

/// Final controller description stored in [`super::VmMachinePlan`].
#[derive(Clone, Debug)]
pub enum InterruptControllerPlan {
    /// Arm GICv3 topology.
    Aarch64GicV3(Aarch64GicV3Plan),
    /// RISC-V PLIC topology.
    RiscvPlic(RiscvPlicPlan),
    /// x86 APIC topology.
    X86Apic(X86ApicPlan),
    /// LoongArch interrupt topology.
    LoongArch(LoongArchInterruptPlan),
}

pub(crate) fn is_planned_guest_firmware_infrastructure(
    plan: Option<&InterruptControllerPlan>,
    compatibles: &[String],
) -> bool {
    if compatibles
        .iter()
        .any(|compatible| compatible == "arm,gic-v3-its")
    {
        return matches!(
            plan,
            Some(InterruptControllerPlan::Aarch64GicV3(gic)) if gic.its().is_some()
        );
    }
    super::is_guest_firmware_infrastructure(compatibles)
}

pub(crate) fn is_planned_host_dependency_substitute(compatibles: &[String]) -> bool {
    // A VM-local software ITS can describe virtual MSI endpoints, but it does
    // not make a passthrough device's physical MSI transactions isolatable.
    // Physical ITS substitution requires a distinct platform capability that
    // AxVM does not expose yet.
    if compatibles
        .iter()
        .any(|compatible| compatible == "arm,gic-v3-its")
    {
        return false;
    }
    super::is_guest_firmware_infrastructure(compatibles)
}

pub(crate) fn resolve_interrupt_controller(
    profile: Option<&InterruptControllerProfile>,
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
) -> MachinePlanResult<Option<InterruptControllerPlan>> {
    profile
        .map(|profile| match profile {
            InterruptControllerProfile::Aarch64GicV3(profile) => {
                resolve_aarch64_gicv3(profile, request, snapshot)
                    .map(InterruptControllerPlan::Aarch64GicV3)
            }
            InterruptControllerProfile::RiscvPlic(profile) => {
                Ok(InterruptControllerPlan::RiscvPlic(RiscvPlicPlan {
                    mmio: profile.mmio,
                    context_count: request.vcpu_count().checked_mul(2).ok_or_else(|| {
                        MachinePlanError::InvalidFirmware {
                            detail: "RISC-V PLIC context count overflows".into(),
                        }
                    })?,
                    source_count: profile.source_count,
                }))
            }
            InterruptControllerProfile::X86Apic(profile) => {
                Ok(InterruptControllerPlan::X86Apic(X86ApicPlan {
                    lapic: profile.lapic,
                    ioapic: profile.ioapic,
                }))
            }
            InterruptControllerProfile::LoongArch(profile) => {
                Ok(InterruptControllerPlan::LoongArch(LoongArchInterruptPlan {
                    pch_pic: profile.pch_pic,
                    pch_msi: profile.pch_msi,
                    routing: profile.routing,
                }))
            }
        })
        .transpose()
}

fn resolve_aarch64_gicv3(
    profile: &Aarch64GicV3Profile,
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
) -> MachinePlanResult<Aarch64GicV3Plan> {
    let (distributor, redistributor_aperture, its) = if request.mode() == VmMachineMode::Passthrough
    {
        let gic = snapshot
            .devices()
            .iter()
            .find(|device| {
                device
                    .compatibles()
                    .iter()
                    .any(|compatible| compatible == "arm,gic-v3")
            })
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "host FDT contains no arm,gic-v3 controller".into(),
            })?;
        let distributor = *gic
            .mmio()
            .first()
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: alloc::format!("host GICv3 node '{}' has no Distributor range", gic.id()),
            })?;
        let redistributors =
            *gic.mmio()
                .get(1)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: alloc::format!(
                        "host GICv3 node '{}' has no Redistributor range",
                        gic.id()
                    ),
                })?;
        let its = snapshot
            .devices()
            .iter()
            .find_map(|device| {
                device
                    .compatibles()
                    .iter()
                    .any(|compatible| compatible == "arm,gic-v3-its")
                    .then(|| device.mmio().first().copied())
                    .flatten()
            })
            .or(profile.its);
        (distributor, redistributors, its)
    } else {
        let maximum_redistributor_size = profile
            .redistributor_stride
            .checked_mul(request.vcpu_count() as u64)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "AArch64 Redistributor range overflows".into(),
            })?;
        (
            profile.distributor,
            AddressRange::new(profile.redistributor_base, maximum_redistributor_size)?,
            profile.its,
        )
    };

    let redistributor_size = profile
        .redistributor_stride
        .checked_mul(request.vcpu_count() as u64)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "AArch64 Redistributor range overflows".into(),
        })?;
    if redistributor_aperture.size() < redistributor_size {
        return Err(MachinePlanError::InvalidFirmware {
            detail: alloc::format!(
                "GICv3 Redistributor aperture {:#x} is too small for {} vCPUs with stride {:#x}",
                redistributor_aperture.size(),
                request.vcpu_count(),
                profile.redistributor_stride
            ),
        });
    }
    Ok(Aarch64GicV3Plan {
        distributor,
        redistributors: AddressRange::new(redistributor_aperture.base(), redistributor_size)?,
        redistributor_stride: profile.redistributor_stride,
        its,
        spi_count: profile.spi_count,
    })
}
