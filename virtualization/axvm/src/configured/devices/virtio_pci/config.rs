use std::format;

use axdevice::{
    PciCapabilityEffectAccess, PciCapabilityEffectRegion, PciCapabilityId, PciCapabilitySnapshot,
    PciCapabilitySpec, PciConfigEffectId, PciResult,
};
use axdevice_base::{AccessWidth, DeviceError, DeviceResult};
use axvirtio_common::pci::{
    VIRTIO_PCI_CAP_VENDOR_SPECIFIC, VIRTIO_PCI_CONFIG_EFFECT_ID, VirtioPciCapabilitySet,
    VirtioPciCapabilityType,
};

use super::{PCI_CFG_DATA_END, PCI_CFG_DATA_OFFSET, PCI_CFG_EFFECTS};

pub(super) fn decode_pci_cfg(
    effect_id: PciConfigEffectId,
    effect_offset: u8,
    width: AccessWidth,
    snapshot: PciCapabilitySnapshot,
) -> DeviceResult<u64> {
    decode_pci_cfg_bytes(effect_id, effect_offset, width, snapshot.bytes())
}

pub(super) fn decode_pci_cfg_bytes(
    effect_id: PciConfigEffectId,
    effect_offset: u8,
    width: AccessWidth,
    bytes: &[u8],
) -> DeviceResult<u64> {
    if effect_id != PCI_CFG_EFFECTS[0] {
        return Err(DeviceError::Unsupported {
            operation: "VirtIO PCI_CFG effect",
            detail: "unknown effect region".into(),
        });
    }
    if bytes.len() < 18 {
        return Err(DeviceError::InvalidData {
            operation: "VirtIO PCI_CFG effect",
            detail: "PCI_CFG capability payload is truncated".into(),
        });
    }
    if bytes[0] != 20 {
        return Err(DeviceError::InvalidData {
            operation: "VirtIO PCI_CFG effect",
            detail: "PCI_CFG capability has an invalid cap_len".into(),
        });
    }
    if bytes[1] != VirtioPciCapabilityType::PciConfig as u8 {
        return Err(DeviceError::InvalidData {
            operation: "VirtIO PCI_CFG effect",
            detail: "effect snapshot is not a PCI_CFG capability".into(),
        });
    }
    let bar = bytes[2];
    let target =
        u32::from_le_bytes(
            bytes[6..10]
                .try_into()
                .map_err(|_| DeviceError::InvalidData {
                    operation: "VirtIO PCI_CFG effect",
                    detail: "PCI_CFG target selector is malformed".into(),
                })?,
        );
    let length =
        u32::from_le_bytes(
            bytes[10..14]
                .try_into()
                .map_err(|_| DeviceError::InvalidData {
                    operation: "VirtIO PCI_CFG effect",
                    detail: "PCI_CFG length selector is malformed".into(),
                })?,
        );
    let width_bytes = width.size() as u32;
    let lane = u32::from(effect_offset);
    let lane_end = lane.checked_add(width_bytes);
    if bar != 0
        || !matches!(width_bytes, 1 | 2 | 4)
        || length != width_bytes
        || lane < PCI_CFG_DATA_OFFSET
        || lane_end.is_none_or(|end| end > PCI_CFG_DATA_END)
    {
        return Err(DeviceError::InvalidInput {
            operation: "VirtIO PCI_CFG effect",
            detail: format!(
                "invalid BAR/length/width/lane selector: bar={bar}, length={length}, lane={lane}"
            ),
        });
    }
    target
        .checked_add(lane - PCI_CFG_DATA_OFFSET)
        .and_then(|target| target.checked_add(width_bytes))
        .filter(|end| *end <= 0x1000)
        .map(|end| u64::from(end - width_bytes))
        .ok_or(DeviceError::InvalidInput {
            operation: "VirtIO PCI_CFG effect",
            detail: "BAR-relative target is outside BAR0".into(),
        })
}

/// Converts a VirtIO capability set into root-owned generic PCI declarations.
///
/// The root derives the serialized `cap_len` and capability chain from these
/// declarations.  The VirtIO transport therefore never owns PCI config-space
/// offsets or a BAR GPA.
///
/// # Errors
///
/// Returns the generic PCI declaration error if an effect region cannot be
/// represented in the serialized capability body.
pub fn virtio_capabilities(
    capabilities: &VirtioPciCapabilitySet,
) -> PciResult<Vec<PciCapabilitySpec>> {
    capabilities
        .as_slice()
        .iter()
        .map(|capability| {
            let body = capability.body();
            let mut write_mask = std::vec![0; body.len()];
            if capability.cfg_type() == VirtioPciCapabilityType::PciConfig {
                // cfg_type and pci_cfg_data are immutable/effect-only. The
                // selector is the root-owned mutable portion of PCI_CFG.
                write_mask[2] = u8::MAX;
                write_mask[6..14].fill(u8::MAX);
            }
            let spec = PciCapabilitySpec::new(
                PciCapabilityId::new(VIRTIO_PCI_CAP_VENDOR_SPECIFIC),
                body.clone().into_boxed_slice(),
                write_mask.into_boxed_slice(),
            )?;
            if capability.cfg_type() == VirtioPciCapabilityType::PciConfig {
                spec.with_effect(PciCapabilityEffectRegion::new(
                    PciConfigEffectId::new(VIRTIO_PCI_CONFIG_EFFECT_ID),
                    PCI_CFG_DATA_OFFSET as u8,
                    4,
                    PciCapabilityEffectAccess::ReadWrite,
                )?)
            } else {
                Ok(spec)
            }
        })
        .collect()
}
