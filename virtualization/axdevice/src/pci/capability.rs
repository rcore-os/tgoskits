//! Generic conventional PCI capability declarations and layouts.

use alloc::{boxed::Box, vec::Vec};

use super::{ConfigOffset, PciError, PciResult, config_layout};

const CAPABILITY_HEADER_SIZE: usize = 2;
const CAPABILITY_BODY_MAX_SIZE: usize = u8::MAX as usize - CAPABILITY_HEADER_SIZE;

/// Identifies one conventional PCI capability type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciCapabilityId(u8);

impl PciCapabilityId {
    /// Creates a capability identifier.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Returns the capability identifier.
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Identifies one endpoint configuration effect within a function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciConfigEffectId(u16);

impl PciConfigEffectId {
    /// Creates an effect identifier owned by one PCI function declaration.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the numeric identifier.
    pub const fn value(self) -> u16 {
        self.0
    }
}

/// Storage behavior of one byte in a capability body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciCapabilityByteMode {
    /// The byte is immutable after the function is created.
    Constant,
    /// The byte is root-owned storage updated by its write mask.
    StoredMasked,
    /// The byte belongs to an endpoint effect and has no config shadow.
    EffectOnly,
}

/// Access directions supported by one endpoint configuration effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciCapabilityEffectAccess {
    /// The effect supports reads only.
    Read,
    /// The effect supports writes only.
    Write,
    /// The effect supports both reads and writes.
    ReadWrite,
}

/// Root-time snapshot of one serialized capability body.
///
/// The snapshot contains the current root-owned body bytes, including bytes
/// that are not part of the endpoint effect itself. An endpoint can therefore
/// validate selector state captured by the same config transaction without
/// rereading mutable root state after the root lock is released.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciCapabilitySnapshot {
    length: u8,
    bytes: [u8; CAPABILITY_BODY_MAX_SIZE],
}

impl PciCapabilitySnapshot {
    pub(crate) fn from_body(body: &[u8]) -> Self {
        debug_assert!(body.len() <= CAPABILITY_BODY_MAX_SIZE);
        let mut bytes = [0; CAPABILITY_BODY_MAX_SIZE];
        bytes[..body.len()].copy_from_slice(body);
        Self {
            length: body.len() as u8,
            bytes,
        }
    }

    /// Returns the serialized body bytes captured for this config effect.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

impl PciCapabilityEffectAccess {
    pub(crate) const fn allows_read(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    pub(crate) const fn allows_write(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

/// One endpoint-owned effect region within a serialized capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciCapabilityEffectRegion {
    effect: PciConfigEffectId,
    offset: u8,
    length: u8,
    access: PciCapabilityEffectAccess,
}

impl PciCapabilityEffectRegion {
    /// Creates an effect region relative to the capability start.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidCapability`] when `length` is zero.
    pub fn new(
        effect: PciConfigEffectId,
        offset: u8,
        length: u8,
        access: PciCapabilityEffectAccess,
    ) -> PciResult<Self> {
        if length == 0 {
            return Err(PciError::InvalidCapability {
                detail: "capability effect region must not be empty".into(),
            });
        }
        Ok(Self {
            effect,
            offset,
            length,
            access,
        })
    }

    /// Returns the effect identifier.
    pub const fn effect(self) -> PciConfigEffectId {
        self.effect
    }

    /// Returns the capability-relative offset.
    pub const fn offset(self) -> u8 {
        self.offset
    }

    /// Returns the effect length.
    pub const fn length(self) -> u8 {
        self.length
    }

    /// Returns the supported access direction.
    pub const fn access(self) -> PciCapabilityEffectAccess {
        self.access
    }
}

/// One generic capability declaration supplied by a PCI function.
///
/// `body` contains the serialized bytes after the conventional capability
/// identifier and next-pointer bytes. The root owns those two header bytes,
/// the assigned config-space offset, and the mutable storage described by
/// `write_mask`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciCapabilitySpec {
    id: PciCapabilityId,
    body: Box<[u8]>,
    write_mask: Box<[u8]>,
    effects: Vec<PciCapabilityEffectRegion>,
}

impl PciCapabilitySpec {
    /// Creates a capability declaration.
    ///
    /// The body and write-mask lengths must match. Effect regions can be
    /// added with [`Self::with_effect`].
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidCapability`] when the body and write-mask
    /// lengths differ or the serialized capability is too large.
    pub fn new(
        id: PciCapabilityId,
        body: impl Into<Box<[u8]>>,
        write_mask: impl Into<Box<[u8]>>,
    ) -> PciResult<Self> {
        let body = body.into();
        let write_mask = write_mask.into();
        if body.len() != write_mask.len() {
            return Err(PciError::InvalidCapability {
                detail: "capability body and write mask lengths differ".into(),
            });
        }
        if body.len() + CAPABILITY_HEADER_SIZE > u8::MAX as usize {
            return Err(PciError::InvalidCapability {
                detail: "capability is too large for a conventional PCI header".into(),
            });
        }
        Ok(Self {
            id,
            body,
            write_mask,
            effects: Vec::new(),
        })
    }

    /// Adds one endpoint effect region.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidCapability`] when the region is outside the
    /// serialized body, overlaps another effect, or has writable bytes.
    pub fn with_effect(mut self, region: PciCapabilityEffectRegion) -> PciResult<Self> {
        validate_effect_region(&self, region)?;
        self.effects.push(region);
        Ok(self)
    }

    /// Returns the capability identifier.
    pub const fn id(&self) -> PciCapabilityId {
        self.id
    }

    /// Returns the serialized body after the two generic header bytes.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns the guest-writable mask for the serialized body.
    pub fn write_mask(&self) -> &[u8] {
        &self.write_mask
    }

    /// Returns endpoint effect regions in declaration order.
    pub fn effects(&self) -> &[PciCapabilityEffectRegion] {
        &self.effects
    }
}

/// One capability after deterministic placement in conventional config space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciCapabilityLayout {
    spec: PciCapabilitySpec,
    offset: ConfigOffset,
}

impl PciCapabilityLayout {
    /// Returns the capability identifier.
    pub const fn id(&self) -> PciCapabilityId {
        self.spec.id()
    }

    /// Returns the assigned config-space offset.
    pub const fn offset(&self) -> ConfigOffset {
        self.offset
    }

    /// Returns the serialized capability length, including its two-byte header.
    pub const fn length(&self) -> u8 {
        (CAPABILITY_HEADER_SIZE + self.spec.body.len()) as u8
    }

    /// Returns the serialized body after the two generic header bytes.
    pub fn body(&self) -> &[u8] {
        self.spec.body()
    }

    /// Returns the guest-writable mask for the serialized body.
    pub fn write_mask(&self) -> &[u8] {
        self.spec.write_mask()
    }

    /// Returns endpoint effect regions in capability-relative coordinates.
    pub fn effects(&self) -> &[PciCapabilityEffectRegion] {
        self.spec.effects()
    }

    pub(crate) fn snapshot(&self, config: &[u8]) -> PciCapabilitySnapshot {
        let start = usize::from(self.offset.value()) + CAPABILITY_HEADER_SIZE;
        let end = start + self.spec.body.len();
        PciCapabilitySnapshot::from_body(&config[start..end])
    }

    /// Returns the storage behavior of a serialized capability byte.
    pub fn byte_mode(&self, config_offset: ConfigOffset) -> Option<PciCapabilityByteMode> {
        let config_offset = usize::from(config_offset.value());
        let relative = config_offset.checked_sub(usize::from(self.offset.value()))?;
        if relative >= usize::from(self.length()) {
            return None;
        }
        if relative < CAPABILITY_HEADER_SIZE {
            return Some(PciCapabilityByteMode::Constant);
        }
        if self.effects().iter().any(|effect| {
            let start = usize::from(effect.offset());
            let end = start + usize::from(effect.length());
            (start..end).contains(&relative)
        }) {
            return Some(PciCapabilityByteMode::EffectOnly);
        }
        let body_offset = relative - CAPABILITY_HEADER_SIZE;
        Some(if self.write_mask()[body_offset] == 0 {
            PciCapabilityByteMode::Constant
        } else {
            PciCapabilityByteMode::StoredMasked
        })
    }

    pub(crate) fn effect_for_access(
        &self,
        config_offset: usize,
        size: usize,
        write: bool,
        width: crate::AccessWidth,
    ) -> PciResult<Option<PciCapabilityEffectRegion>> {
        let capability_start = usize::from(self.offset.value());
        let capability_end = capability_start + usize::from(self.length());
        let access_end = config_offset
            .checked_add(size)
            .ok_or(PciError::InvalidConfigAccess {
                offset: config_offset as u16,
                width,
                detail: "capability access range overflows",
            })?;
        let mut matched = None;
        for effect in &self.spec.effects {
            let start = capability_start + usize::from(effect.offset());
            let end = start + usize::from(effect.length());
            if config_offset < end && start < access_end {
                let contained = start <= config_offset && access_end <= end;
                let allowed = if write {
                    effect.access().allows_write()
                } else {
                    effect.access().allows_read()
                };
                if !contained || !allowed {
                    return Err(PciError::InvalidConfigAccess {
                        offset: config_offset as u16,
                        width,
                        detail: "config access partially covers or uses an unsupported capability \
                                 effect",
                    });
                }
                if matched.replace(*effect).is_some() {
                    return Err(PciError::InvalidConfigAccess {
                        offset: config_offset as u16,
                        width,
                        detail: "config access covers multiple capability effects",
                    });
                }
            }
        }
        if config_offset < capability_end
            && capability_start < access_end
            && (config_offset < capability_start || capability_end < access_end)
            && matched.is_some()
        {
            return Err(PciError::InvalidConfigAccess {
                offset: config_offset as u16,
                width,
                detail: "config access crosses a capability boundary",
            });
        }
        Ok(matched)
    }

    pub(crate) fn intersects_effect(&self, config_offset: usize, size: usize) -> bool {
        let capability_start = usize::from(self.offset.value());
        let access_end = config_offset.saturating_add(size);
        self.spec.effects.iter().any(|effect| {
            let start = capability_start + usize::from(effect.offset());
            let end = start + usize::from(effect.length());
            config_offset < end && start < access_end
        })
    }
}

pub(crate) fn layout_capabilities(
    specifications: &[PciCapabilitySpec],
) -> PciResult<Vec<PciCapabilityLayout>> {
    let mut layouts = Vec::with_capacity(specifications.len());
    let mut cursor = config_layout::CONFIG_STANDARD_HEADER_END;
    let mut effect_ids = Vec::new();
    for spec in specifications {
        for effect in spec.effects() {
            if effect_ids.contains(&effect.effect()) {
                return Err(PciError::InvalidCapability {
                    detail: "config effect identifier is duplicated within a function".into(),
                });
            }
            effect_ids.push(effect.effect());
        }
        let offset = align_up(cursor, 4);
        let length = CAPABILITY_HEADER_SIZE
            .checked_add(spec.body().len())
            .ok_or(PciError::InvalidCapability {
                detail: "capability length overflows conventional config space".into(),
            })?;
        let end = offset
            .checked_add(length)
            .ok_or(PciError::InvalidCapability {
                detail: "capability placement overflows conventional config space".into(),
            })?;
        if end > config_layout::CONFIG_SPACE_SIZE {
            return Err(PciError::InvalidCapability {
                detail: "capability declarations exceed conventional config space".into(),
            });
        }
        let offset = ConfigOffset::new(offset as u16)?;
        layouts.push(PciCapabilityLayout {
            spec: spec.clone(),
            offset,
        });
        cursor = end;
    }
    Ok(layouts)
}

fn validate_effect_region(
    spec: &PciCapabilitySpec,
    region: PciCapabilityEffectRegion,
) -> PciResult {
    let start = usize::from(region.offset());
    let end =
        start
            .checked_add(usize::from(region.length()))
            .ok_or(PciError::InvalidCapability {
                detail: "capability effect range overflows".into(),
            })?;
    let capability_end = CAPABILITY_HEADER_SIZE + spec.body.len();
    if start < CAPABILITY_HEADER_SIZE || end > capability_end {
        return Err(PciError::InvalidCapability {
            detail: "capability effect range is outside the serialized capability".into(),
        });
    }
    if spec.effects.iter().any(|existing| {
        let existing_start = usize::from(existing.offset());
        let existing_end = existing_start + usize::from(existing.length());
        start < existing_end && existing_start < end
    }) {
        return Err(PciError::InvalidCapability {
            detail: "capability effect regions overlap".into(),
        });
    }
    for offset in start..end {
        if spec.write_mask[offset - CAPABILITY_HEADER_SIZE] != 0 {
            return Err(PciError::InvalidCapability {
                detail: "effect-only capability bytes must have a zero write mask".into(),
            });
        }
    }
    Ok(())
}

const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn lays_out_capabilities_in_declaration_order_and_alignment() {
        let first =
            PciCapabilitySpec::new(PciCapabilityId::new(1), vec![1, 2], vec![0, 0]).unwrap();
        let second = PciCapabilitySpec::new(
            PciCapabilityId::new(2),
            vec![3, 4, 5, 6],
            vec![0xff, 0, 0, 0],
        )
        .unwrap();

        let layouts = layout_capabilities(&[first, second]).unwrap();
        assert_eq!(layouts[0].offset().value(), 0x40);
        assert_eq!(layouts[0].length(), 4);
        assert_eq!(
            layouts[0].byte_mode(ConfigOffset::new(0x40).unwrap()),
            Some(PciCapabilityByteMode::Constant)
        );
        assert_eq!(layouts[1].offset().value(), 0x44);
        assert_eq!(layouts[1].length(), 6);
        assert_eq!(
            layouts[1].byte_mode(ConfigOffset::new(0x46).unwrap()),
            Some(PciCapabilityByteMode::StoredMasked)
        );
        assert_eq!(
            layouts[1].byte_mode(ConfigOffset::new(0x47).unwrap()),
            Some(PciCapabilityByteMode::Constant)
        );
    }

    #[test]
    fn rejects_invalid_effect_regions_and_duplicate_effect_ids() {
        let effect = PciCapabilityEffectRegion::new(
            PciConfigEffectId::new(7),
            2,
            2,
            PciCapabilityEffectAccess::ReadWrite,
        )
        .unwrap();
        let overlapping = PciCapabilityEffectRegion::new(
            PciConfigEffectId::new(8),
            3,
            1,
            PciCapabilityEffectAccess::Read,
        )
        .unwrap();
        let spec = PciCapabilitySpec::new(PciCapabilityId::new(9), vec![0; 4], vec![0; 4])
            .unwrap()
            .with_effect(effect)
            .unwrap();
        assert!(matches!(
            spec.clone().with_effect(overlapping),
            Err(PciError::InvalidCapability { .. })
        ));
        let duplicate = PciCapabilityEffectRegion::new(
            PciConfigEffectId::new(7),
            4,
            1,
            PciCapabilityEffectAccess::Read,
        )
        .unwrap();
        assert!(matches!(
            layout_capabilities(&[spec.clone().with_effect(duplicate).unwrap()]),
            Err(PciError::InvalidCapability { .. })
        ));

        assert_eq!(
            layout_capabilities(&[spec]).unwrap()[0].byte_mode(ConfigOffset::new(0x42).unwrap()),
            Some(PciCapabilityByteMode::EffectOnly)
        );
    }

    #[test]
    fn rejects_invalid_capability_sizes_and_effect_bounds() {
        assert!(matches!(
            PciCapabilitySpec::new(PciCapabilityId::new(1), vec![0], vec![]),
            Err(PciError::InvalidCapability { .. })
        ));
        assert!(matches!(
            PciCapabilitySpec::new(
                PciCapabilityId::new(1),
                vec![0; CAPABILITY_BODY_MAX_SIZE + 1],
                vec![0; CAPABILITY_BODY_MAX_SIZE + 1],
            ),
            Err(PciError::InvalidCapability { .. })
        ));
        assert!(matches!(
            PciCapabilityEffectRegion::new(
                PciConfigEffectId::new(1),
                2,
                0,
                PciCapabilityEffectAccess::Read,
            ),
            Err(PciError::InvalidCapability { .. })
        ));

        let out_of_bounds = PciCapabilityEffectRegion::new(
            PciConfigEffectId::new(1),
            6,
            1,
            PciCapabilityEffectAccess::Read,
        )
        .unwrap();
        assert!(matches!(
            PciCapabilitySpec::new(PciCapabilityId::new(1), vec![0; 4], vec![0; 4])
                .unwrap()
                .with_effect(out_of_bounds),
            Err(PciError::InvalidCapability { .. })
        ));

        let conventional_space_exhausted = PciCapabilitySpec::new(
            PciCapabilityId::new(1),
            vec![0; CAPABILITY_BODY_MAX_SIZE],
            vec![0; CAPABILITY_BODY_MAX_SIZE],
        )
        .unwrap();
        assert!(matches!(
            layout_capabilities(&[conventional_space_exhausted]),
            Err(PciError::InvalidCapability { .. })
        ));
    }
}
