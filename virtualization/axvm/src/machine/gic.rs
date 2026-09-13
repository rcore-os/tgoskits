//! Host-derived guest GIC and ITS firmware identity.

use std::{string::String, vec::Vec};

use axdevice_base::ItsId;

use super::GuestMmioRegion;

/// Default architectural spacing between GICv3 Redistributor frames.
pub(crate) const AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE: usize = 0x2_0000;
const GICV2_DISTRIBUTOR_SIZE: usize = 0x1_000;
const GICV3_DISTRIBUTOR_MINIMUM_SIZE: usize = 0x1_0000;
const GICC_MINIMUM_SIZE: usize = 0x2_000;
const GICR_ALIGNMENT: usize = 0x1_0000;

/// Host-derived GICv3 Redistributor layout exposed to the guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestGicRedistributorProfile {
    /// One or more guest-visible Redistributor register regions.
    pub regions: Vec<GuestMmioRegion>,
    /// Byte stride between consecutive Redistributor frames.
    pub stride: usize,
}

/// Per-CPU resources exposed by the selected GIC model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuestGicCpuRegion {
    /// GICv2 memory-mapped CPU interface.
    CpuInterface(GuestMmioRegion),
    /// GICv3 Redistributor frames.
    Redistributors(GuestGicRedistributorProfile),
}

/// One host-derived ITS instance retained for the guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestItsProfile {
    /// Stable VM-local ITS identifier.
    pub id: ItsId,
    /// Absolute path of the host ITS node.
    pub node_path: String,
    /// ITS node phandle, when supplied by firmware.
    pub node_phandle: Option<u32>,
    /// Guest-visible ITS register aperture.
    pub registers: GuestMmioRegion,
}

/// Host firmware resources retained by the virtual GIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestGicProfile {
    /// Compatible string identifying the selected GIC register model.
    pub compatible: String,
    /// Absolute path of the host GIC node.
    pub node_path: String,
    /// GIC node phandle, when supplied by firmware.
    pub node_phandle: Option<u32>,
    /// Guest-visible distributor registers.
    pub distributor: GuestMmioRegion,
    /// Guest-visible per-CPU registers.
    pub cpu_region: GuestGicCpuRegion,
    /// Guest-visible ITS instances, empty for GICv2 or hosts without ITS.
    pub its: Vec<GuestItsProfile>,
}

impl GuestGicProfile {
    /// Normalizes host register windows to the architectural guest-visible
    /// frames, then validates the complete profile.
    pub(crate) fn normalized_for_vcpus(
        mut self,
        vcpu_count: usize,
    ) -> Result<Self, GuestGicProfileError> {
        if let GuestGicCpuRegion::CpuInterface(region) = &mut self.cpu_region {
            if self.distributor.length < GICV2_DISTRIBUTOR_SIZE {
                return Err(GuestGicProfileError::DistributorTooSmall {
                    length: self.distributor.length,
                    minimum: GICV2_DISTRIBUTOR_SIZE,
                });
            }
            if region.length < GICC_MINIMUM_SIZE {
                return Err(GuestGicProfileError::CpuInterfaceTooSmall {
                    length: region.length,
                });
            }
            self.distributor.length = GICV2_DISTRIBUTOR_SIZE;
            region.length = GICC_MINIMUM_SIZE;
        }
        self.validate_for_vcpus(vcpu_count)?;
        Ok(self)
    }

    /// Validates host-derived register geometry for the requested vCPU count.
    pub(crate) fn validate_for_vcpus(&self, vcpu_count: usize) -> Result<(), GuestGicProfileError> {
        let distributor_minimum = match &self.cpu_region {
            GuestGicCpuRegion::CpuInterface(_) => GICV2_DISTRIBUTOR_SIZE,
            GuestGicCpuRegion::Redistributors(_) => GICV3_DISTRIBUTOR_MINIMUM_SIZE,
        };
        if self.distributor.length < distributor_minimum {
            return Err(GuestGicProfileError::DistributorTooSmall {
                length: self.distributor.length,
                minimum: distributor_minimum,
            });
        }
        let mut windows = Vec::with_capacity(2 + self.its.len());
        push_window(&mut windows, "distributor", 0, self.distributor)?;
        match &self.cpu_region {
            GuestGicCpuRegion::CpuInterface(region) => {
                if region.length < GICC_MINIMUM_SIZE {
                    return Err(GuestGicProfileError::CpuInterfaceTooSmall {
                        length: region.length,
                    });
                }
                if !self.its.is_empty() {
                    return Err(GuestGicProfileError::ItsRequiresGicV3);
                }
                push_window(&mut windows, "CPU interface", 0, *region)?;
            }
            GuestGicCpuRegion::Redistributors(redistributors) => {
                redistributors.validate_for_vcpus(vcpu_count)?;
                for (index, region) in redistributors.regions.iter().copied().enumerate() {
                    push_window(&mut windows, "Redistributor", index, region)?;
                }
            }
        }
        for (index, its) in self.its.iter().enumerate() {
            if its.registers.length == 0 {
                return Err(GuestGicProfileError::EmptyRegion {
                    resource: "ITS",
                    index,
                });
            }
            if self.its[..index]
                .iter()
                .any(|existing| existing.id == its.id)
            {
                return Err(GuestGicProfileError::DuplicateItsId { id: its.id });
            }
            push_window(&mut windows, "ITS", index, its.registers)?;
        }
        validate_non_overlapping(&windows)
    }
}

impl GuestGicRedistributorProfile {
    fn validate_for_vcpus(&self, vcpu_count: usize) -> Result<(), GuestGicProfileError> {
        if self.regions.is_empty() {
            return Err(GuestGicProfileError::MissingRedistributors);
        }
        if self.stride < AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE
            || !self.stride.is_multiple_of(GICR_ALIGNMENT)
        {
            return Err(GuestGicProfileError::InvalidRedistributorStride {
                stride: self.stride,
            });
        }

        let mut frame_count = 0usize;
        for (index, region) in self.regions.iter().enumerate() {
            if region.length == 0
                || !region.base.is_multiple_of(GICR_ALIGNMENT)
                || !region.length.is_multiple_of(GICR_ALIGNMENT)
            {
                return Err(GuestGicProfileError::InvalidRedistributorRegion {
                    index,
                    base: region.base,
                    length: region.length,
                });
            }
            frame_count = frame_count
                .checked_add(region.length / self.stride)
                .ok_or(GuestGicProfileError::RedistributorCapacityOverflow)?;
        }
        if frame_count < vcpu_count {
            return Err(GuestGicProfileError::InsufficientRedistributors {
                available: frame_count,
                required: vcpu_count,
            });
        }
        Ok(())
    }
}

type NamedWindow = (&'static str, usize, GuestMmioRegion);

fn push_window(
    windows: &mut Vec<NamedWindow>,
    resource: &'static str,
    index: usize,
    region: GuestMmioRegion,
) -> Result<(), GuestGicProfileError> {
    region
        .base
        .checked_add(region.length)
        .ok_or(GuestGicProfileError::RegionEndOverflow {
            resource,
            index,
            base: region.base,
            length: region.length,
        })?;
    windows.push((resource, index, region));
    Ok(())
}

fn validate_non_overlapping(windows: &[NamedWindow]) -> Result<(), GuestGicProfileError> {
    for (position, (first_resource, first_index, first)) in windows.iter().enumerate() {
        let first_end = first.base + first.length;
        for (second_resource, second_index, second) in &windows[position + 1..] {
            let second_end = second.base + second.length;
            if first.base < second_end && second.base < first_end {
                return Err(GuestGicProfileError::OverlappingRegions {
                    first_resource,
                    first_index: *first_index,
                    second_resource,
                    second_index: *second_index,
                });
            }
        }
    }
    Ok(())
}

/// Invalid host-derived GIC firmware geometry.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GuestGicProfileError {
    /// The distributor cannot expose the mandatory architectural frame.
    #[error("AArch64 GIC distributor window {length:#x} is smaller than {minimum:#x}")]
    DistributorTooSmall { length: usize, minimum: usize },
    /// The GICv2 CPU interface cannot expose the mandatory architectural frame.
    #[error("AArch64 GIC CPU-interface window {length:#x} is smaller than {GICC_MINIMUM_SIZE:#x}")]
    CpuInterfaceTooSmall { length: usize },
    /// GICv3 requires at least one Redistributor region.
    #[error("AArch64 GICv3 has no Redistributor regions")]
    MissingRedistributors,
    /// Redistributor frames require an aligned architectural stride.
    #[error("AArch64 GIC Redistributor stride {stride:#x} is invalid")]
    InvalidRedistributorStride { stride: usize },
    /// One Redistributor region violates the 64-KiB register geometry.
    #[error(
        "AArch64 GIC Redistributor region {index} at {base:#x} with length {length:#x} is not \
         64-KiB aligned"
    )]
    InvalidRedistributorRegion {
        index: usize,
        base: usize,
        length: usize,
    },
    /// The aggregate Redistributor frame count overflowed the host address width.
    #[error("AArch64 GIC Redistributor capacity overflows usize")]
    RedistributorCapacityOverflow,
    /// The host regions do not contain one Redistributor frame per vCPU.
    #[error(
        "AArch64 GIC provides {available} Redistributor frames for {required} configured vCPUs"
    )]
    InsufficientRedistributors { available: usize, required: usize },
    /// ITS is a GICv3-only facility.
    #[error("AArch64 GICv2 firmware profile cannot contain ITS instances")]
    ItsRequiresGicV3,
    /// A firmware register aperture must contain at least one byte.
    #[error("AArch64 {resource} region {index} is empty")]
    EmptyRegion {
        resource: &'static str,
        index: usize,
    },
    /// A register aperture cannot wrap around the host address width.
    #[error(
        "AArch64 {resource} region {index} at {base:#x} with length {length:#x} overflows usize"
    )]
    RegionEndOverflow {
        resource: &'static str,
        index: usize,
        base: usize,
        length: usize,
    },
    /// Guest-visible GIC register apertures must be disjoint.
    #[error(
        "AArch64 {first_resource} region {first_index} overlaps {second_resource} region \
         {second_index}"
    )]
    OverlappingRegions {
        first_resource: &'static str,
        first_index: usize,
        second_resource: &'static str,
        second_index: usize,
    },
    /// VM-local ITS identifiers must be unique.
    #[error("AArch64 GIC profile contains duplicate ITS identifier {id:?}")]
    DuplicateItsId { id: ItsId },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gicv2_normalization_retains_only_guest_visible_frames() {
        let profile = GuestGicProfile {
            compatible: "arm,gic-400".into(),
            node_path: "/interrupt-controller@2a701000".into(),
            node_phandle: Some(1),
            distributor: GuestMmioRegion {
                base: 0x2a70_1000,
                length: 0x1_0000,
            },
            cpu_region: GuestGicCpuRegion::CpuInterface(GuestMmioRegion {
                base: 0x2a70_2000,
                length: 0x1_0000,
            }),
            its: Vec::new(),
        };

        let normalized = profile.normalized_for_vcpus(1).unwrap();

        assert_eq!(normalized.distributor.length, GICV2_DISTRIBUTOR_SIZE);
        assert_eq!(
            normalized.cpu_region,
            GuestGicCpuRegion::CpuInterface(GuestMmioRegion {
                base: 0x2a70_2000,
                length: GICC_MINIMUM_SIZE,
            })
        );
    }
}
