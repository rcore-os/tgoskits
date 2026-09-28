//! Static BAR2 layout of one ivshmem link.
//!
//! The layout object is the single source of truth for how BAR2 is divided:
//! the state table, the optional common section, and the per-peer output
//! sections all derive from it, and F6's stage-2 mapping plans read the same
//! object. It lives in `axdevice::ivshmem` because every peer of one link
//! must observe the same division.

use alloc::{boxed::Box, format, string::String, vec::Vec};
use core::fmt;

use super::{
    error::IvshmemError,
    link::{LinkProfile, PeerId},
};

/// Owner-aware classification of one BAR2 offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bar2Section {
    /// Host-owned state table; guests read it, BAR0 State writes move it.
    StateTable,
    /// Read/write area shared by every peer; absent in the F5 profile.
    Common,
    /// The output section of one peer; read-only for everyone else.
    Output(PeerId),
    /// Page-rounded leftover; reads zero, writes are ignored.
    Reserved,
}

impl Bar2Section {
    /// Returns whether `peer` may write this section.
    ///
    /// StateTable is never writable through BAR2, Common is writable by
    /// every peer, `Output(p)` only by `p`, Reserved never (the caller
    /// ignores those writes silently instead of denying them).
    pub const fn allows_write(self, peer: PeerId) -> bool {
        match self {
            Bar2Section::StateTable | Bar2Section::Reserved => false,
            Bar2Section::Common => true,
            Bar2Section::Output(owner) => owner.value() == peer.value(),
        }
    }

    /// Returns the stable section name used in diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Bar2Section::StateTable => "state table",
            Bar2Section::Common => "common section",
            Bar2Section::Output(_) => "output section",
            Bar2Section::Reserved => "reserved region",
        }
    }
}

/// One aligned section of the shared BAR2 region.
///
/// Both bounds are 4 KiB aligned on purpose: F6 enforces per-section stage-2
/// permissions at page granularity, so a section that is not page aligned can
/// never be mapped without sharing a page with a neighbour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionDesc {
    offset: u64,
    size: u64,
}

impl SectionDesc {
    /// The alignment every section must keep (stage-2 page granularity).
    pub(crate) const ALIGNMENT: u64 = 0x1000;

    /// Creates one section, rejecting unaligned bounds, a size of zero and
    /// overflow of `offset + size`.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidLayout`] when `offset` or `size` is not
    /// a 4 KiB multiple, `size` is zero, or the range overflows `u64`.
    pub fn new(offset: u64, size: u64) -> Result<Self, IvshmemError> {
        let reject = |detail: String| IvshmemError::InvalidLayout { detail };
        if !offset.is_multiple_of(Self::ALIGNMENT) {
            return Err(reject(format!(
                "section offset {offset:#x} is not {:#x}-aligned",
                Self::ALIGNMENT
            )));
        }
        if !size.is_multiple_of(Self::ALIGNMENT) {
            return Err(reject(format!(
                "section size {size:#x} is not {:#x}-aligned",
                Self::ALIGNMENT
            )));
        }
        if size == 0 {
            return Err(reject("section size is zero".into()));
        }
        offset.checked_add(size).ok_or_else(|| {
            reject(format!(
                "section offset {offset:#x} + size {size:#x} overflows u64"
            ))
        })?;
        Ok(Self { offset, size })
    }

    /// Returns the section start offset inside BAR2.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the section size in bytes.
    pub const fn size(self) -> u64 {
        self.size
    }

    /// Returns whether `offset` lies inside the section.
    pub const fn contains(self, offset: u64) -> bool {
        offset >= self.offset && offset - self.offset < self.size
    }
}

/// Static layout of the shared BAR2 region of one link.
pub struct IvshmemMemoryLayout {
    state_table: SectionDesc,
    common: Option<SectionDesc>,
    outputs: Box<[SectionDesc]>,
    bar2_size: u64,
}

impl fmt::Debug for IvshmemMemoryLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IvshmemMemoryLayout")
            .field("state_table", &self.state_table)
            .field("common", &self.common)
            .field("outputs", &self.outputs)
            .field("bar2_size", &self.bar2_size)
            .finish()
    }
}

impl IvshmemMemoryLayout {
    /// Size of one peer's state-table entry in bytes.
    const STATE_ENTRY_SIZE: u64 = 4;

    /// Derives the layout from the link profile.
    ///
    /// Rules: one state-table page at offset 0 with `profile.max_peers()`
    /// entries; a common section when `profile.common_size()` is nonzero,
    /// placed after the state table; one output section per peer of
    /// `profile.output_size()`, in ascending `PeerId` order; leftover pages
    /// stay unassigned (classified as Reserved). `outputs.len()` always
    /// equals `profile.max_peers()`.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidLayout`] when the profile cannot fit
    /// into `bar2_size`.
    pub fn derive(bar2_size: u64, profile: LinkProfile) -> Result<Self, IvshmemError> {
        let max_peers = profile.max_peers();
        let reject = |detail: String| IvshmemError::InvalidLayout { detail };
        if !bar2_size.is_multiple_of(SectionDesc::ALIGNMENT) {
            return Err(reject(format!(
                "BAR2 size {bar2_size:#x} is not {:#x}-aligned",
                SectionDesc::ALIGNMENT
            )));
        }
        let state_table = SectionDesc::new(0, SectionDesc::ALIGNMENT)?;
        if bar2_size < state_table.size() {
            return Err(reject(format!(
                "BAR2 size {bar2_size:#x} cannot host the {:#x} state-table page",
                state_table.size()
            )));
        }
        let entries_end = state_table.offset + u64::from(max_peers) * Self::STATE_ENTRY_SIZE;
        if entries_end > state_table.size() {
            return Err(reject(format!(
                "{max_peers} state entries end at {entries_end:#x} beyond the {:#x} state-table \
                 page",
                state_table.size()
            )));
        }
        let mut cursor = state_table.size();
        let common = if profile.common_size() > 0 {
            let common = SectionDesc::new(cursor, profile.common_size())?;
            cursor += common.size();
            Some(common)
        } else {
            None
        };
        let mut outputs = Vec::new();
        outputs
            .try_reserve_exact(usize::from(max_peers))
            .map_err(|_| IvshmemError::AllocationFailed {
                operation: "derive ivshmem BAR2 layout",
            })?;
        for _peer in 0..u64::from(max_peers) {
            outputs.push(SectionDesc::new(cursor, profile.output_size())?);
            cursor += profile.output_size();
        }
        if cursor > bar2_size {
            return Err(reject(format!(
                "profile sections end at {cursor:#x} beyond the {bar2_size:#x} BAR2"
            )));
        }
        Ok(Self {
            state_table,
            common,
            outputs: outputs.into_boxed_slice(),
            bar2_size,
        })
    }

    /// Returns the state-table section.
    pub const fn state_table(&self) -> SectionDesc {
        self.state_table
    }

    /// Returns the common section, or `None` while the profile has none.
    pub const fn common(&self) -> Option<SectionDesc> {
        self.common
    }

    /// Returns the per-peer output sections indexed by `PeerId::value()`;
    /// the slice length always equals the peer count of the profile.
    pub fn outputs(&self) -> &[SectionDesc] {
        &self.outputs
    }

    /// Returns the total BAR2 size this layout was derived from.
    pub const fn bar2_size(&self) -> u64 {
        self.bar2_size
    }

    /// Returns the offset of one peer's state-table entry.
    ///
    /// The entry arithmetic lives here so links and tests never repeat the
    /// `table + peer * 4` formula.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerOutOfProfile`] when `peer` is at or beyond
    /// the peer count this layout was derived from.
    pub fn state_entry_offset(&self, peer: PeerId, max_peers: u16) -> Result<u64, IvshmemError> {
        if peer.value() >= max_peers {
            return Err(IvshmemError::PeerOutOfProfile {
                peer: peer.value(),
                max_peers,
            });
        }
        Ok(self.state_table.offset() + u64::from(peer.value()) * Self::STATE_ENTRY_SIZE)
    }

    /// Classifies one BAR2 offset into its owning section.
    ///
    /// Pure lookup: no locking, no allocation, and the same result for every
    /// peer of the link (ownership is applied afterwards via
    /// [`Bar2Section::allows_write`]). Offsets past the classified sections
    /// classify as `Reserved`.
    pub fn classify(&self, offset: u64) -> Bar2Section {
        if self.state_table.contains(offset) {
            return Bar2Section::StateTable;
        }
        if let Some(common) = self.common
            && common.contains(offset)
        {
            return Bar2Section::Common;
        }
        for (index, section) in self.outputs.iter().enumerate() {
            if section.contains(offset) {
                return Bar2Section::Output(PeerId::new(index as u16));
            }
        }
        Bar2Section::Reserved
    }

    /// Re-checks the frozen invariants of this layout.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidLayout`] when sections overlap, are
    /// misaligned, or leave `bar2_size`. The peer-count crosscheck of
    /// `outputs.len()` joins with F8's profile-driven derivation, which is
    /// the first caller that can construct a layout with a wrong count.
    pub fn validate(&self) -> Result<(), IvshmemError> {
        let reject = |detail: String| IvshmemError::InvalidLayout { detail };
        let mut sections = Vec::new();
        sections
            .try_reserve_exact(2 + self.outputs.len())
            .map_err(|_| IvshmemError::AllocationFailed {
                operation: "validate ivshmem BAR2 layout",
            })?;
        sections.push((self.state_table, "state table"));
        if let Some(common) = self.common {
            sections.push((common, "common section"));
        }
        for (index, output) in self.outputs.iter().enumerate() {
            sections.push((
                *output,
                Bar2Section::Output(PeerId::new(index as u16)).name(),
            ));
        }
        for (section, name) in &sections {
            if section.offset() + section.size() > self.bar2_size {
                return Err(reject(format!(
                    "{name} at {:#x}..{:#x} leaves the {:#x} BAR2",
                    section.offset(),
                    section.offset() + section.size(),
                    self.bar2_size
                )));
            }
        }
        for (index, (section, name)) in sections.iter().enumerate() {
            for (other, other_name) in sections.iter().skip(index + 1) {
                if section.offset() < other.offset() + other.size()
                    && other.offset() < section.offset() + section.size()
                {
                    return Err(reject(format!(
                        "{name} at {:#x}..{:#x} overlaps {other_name} at {:#x}..{:#x}",
                        section.offset(),
                        section.offset() + section.size(),
                        other.offset(),
                        other.offset() + other.size(),
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::ToString, vec};

    use super::*;
    use crate::ivshmem::SHARED_MEMORY_SIZE;

    fn two_peer_layout() -> IvshmemMemoryLayout {
        IvshmemMemoryLayout::derive(SHARED_MEMORY_SIZE, LinkProfile::baseline()).unwrap()
    }

    #[test]
    fn derives_the_frozen_two_peer_profile() {
        let layout = two_peer_layout();
        assert_eq!(layout.bar2_size(), SHARED_MEMORY_SIZE);
        assert_eq!(layout.state_table().offset(), 0);
        assert_eq!(layout.state_table().size(), 0x1000);
        assert_eq!(layout.common(), None);
        let outputs = layout.outputs();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].offset(), 0x1000);
        assert_eq!(outputs[0].size(), 0x7000);
        assert_eq!(outputs[1].offset(), 0x8000);
        assert_eq!(outputs[1].size(), 0x7000);
        // Both peer entries fit inside the state-table page.
        assert_eq!(layout.state_entry_offset(PeerId::new(0), 2).unwrap(), 0);
        assert_eq!(layout.state_entry_offset(PeerId::new(1), 2).unwrap(), 4);
        layout.validate().unwrap();
    }

    #[test]
    fn classifies_every_offset_boundary_into_its_section() {
        let layout = two_peer_layout();
        let expectations: &[(u64, Bar2Section)] = &[
            (0, Bar2Section::StateTable),
            (4, Bar2Section::StateTable),
            (0xfff, Bar2Section::StateTable),
            (0x1000, Bar2Section::Output(PeerId::new(0))),
            (0x7fff, Bar2Section::Output(PeerId::new(0))),
            (0x8000, Bar2Section::Output(PeerId::new(1))),
            (0xefff, Bar2Section::Output(PeerId::new(1))),
            (0xf000, Bar2Section::Reserved),
            (0xffff, Bar2Section::Reserved),
        ];
        for (offset, expected) in expectations {
            assert_eq!(layout.classify(*offset), *expected, "offset {offset:#x}");
        }
    }

    #[test]
    fn write_permissions_follow_the_owner_matrix() {
        let peer0 = PeerId::new(0);
        let peer1 = PeerId::new(1);
        // The state table is never writable through BAR2.
        assert!(!Bar2Section::StateTable.allows_write(peer0));
        assert!(!Bar2Section::StateTable.allows_write(peer1));
        // Common would be writable by everyone (absent in this profile).
        assert!(Bar2Section::Common.allows_write(peer0));
        assert!(Bar2Section::Common.allows_write(peer1));
        // Output sections are writable by their owner only.
        assert!(Bar2Section::Output(peer0).allows_write(peer0));
        assert!(!Bar2Section::Output(peer0).allows_write(peer1));
        assert!(Bar2Section::Output(peer1).allows_write(peer1));
        assert!(!Bar2Section::Output(peer1).allows_write(peer0));
        // Reserved writes are ignored by the caller, never granted.
        assert!(!Bar2Section::Reserved.allows_write(peer0));
        // Section names are stable, non-empty diagnostics.
        assert!(!Bar2Section::StateTable.name().is_empty());
        assert_eq!(Bar2Section::Output(peer1).name(), "output section");
    }

    #[test]
    fn rejects_bar2_sizes_that_cannot_host_the_sections() {
        // Zero bytes cannot host the state-table page (any page-aligned size
        // below one page is zero).
        assert!(
            IvshmemMemoryLayout::derive(0, LinkProfile::baseline())
                .unwrap_err()
                .to_string()
                .contains("cannot host")
        );
        // Not enough room for the baseline output sections.
        assert!(
            IvshmemMemoryLayout::derive(0x2000, LinkProfile::baseline())
                .unwrap_err()
                .to_string()
                .contains("beyond the")
        );
        // Unaligned BAR2 size.
        assert!(IvshmemMemoryLayout::derive(0x1800, LinkProfile::baseline()).is_err());
        // Empty peer profile is rejected by the profile itself.
        assert!(LinkProfile::new(0, 0, 0x1000).is_err());
        // A reduced profile fits a smaller BAR2: state page plus one output
        // page per peer, with no reserved tail.
        let tight =
            IvshmemMemoryLayout::derive(0x3000, LinkProfile::new(2, 0, 0x1000).unwrap()).unwrap();
        assert_eq!(tight.outputs()[0].size(), 0x1000);
        assert_eq!(tight.outputs()[1].size(), 0x1000);
        assert_eq!(tight.classify(0x3000), Bar2Section::Reserved);
    }

    #[test]
    fn rejects_entries_outside_the_derived_profile() {
        let layout = two_peer_layout();
        assert_eq!(
            layout.state_entry_offset(PeerId::new(2), 2),
            Err(IvshmemError::PeerOutOfProfile {
                peer: 2,
                max_peers: 2
            })
        );
    }

    #[test]
    fn sections_reject_unaligned_or_empty_bounds() {
        let expected_detail = |detail: &str| IvshmemError::InvalidLayout {
            detail: detail.into(),
        };
        assert_eq!(
            SectionDesc::new(0x800, 0x1000),
            Err(expected_detail(
                "section offset 0x800 is not 0x1000-aligned"
            ))
        );
        assert_eq!(
            SectionDesc::new(0, 0x800),
            Err(expected_detail("section size 0x800 is not 0x1000-aligned"))
        );
        assert_eq!(
            SectionDesc::new(0, 0),
            Err(expected_detail("section size is zero"))
        );
    }

    #[test]
    fn section_contains_its_range_exclusively() {
        let section = SectionDesc::new(0x1000, 0x2000).unwrap();
        assert!(section.contains(0x1000));
        assert!(section.contains(0x2fff));
        assert!(!section.contains(0xfff));
        assert!(!section.contains(0x3000));
    }

    #[test]
    fn validate_rejects_overlaps_and_sections_leaving_the_bar2() {
        // A hand-built layout whose second output extends past the BAR2.
        let layout = IvshmemMemoryLayout {
            state_table: SectionDesc::new(0, 0x1000).unwrap(),
            common: None,
            outputs: vec![
                SectionDesc::new(0x1000, 0x7000).unwrap(),
                SectionDesc::new(0x8000, 0x9000).unwrap(),
            ]
            .into_boxed_slice(),
            bar2_size: SHARED_MEMORY_SIZE,
        };
        let error = layout.validate().unwrap_err();
        assert!(error.to_string().contains("leaves the"));
        assert_eq!(layout.classify(0xf000), Bar2Section::Output(PeerId::new(1)));

        // A hand-built layout with overlapping sections.
        let layout = IvshmemMemoryLayout {
            state_table: SectionDesc::new(0, 0x1000).unwrap(),
            common: Some(SectionDesc::new(0x1000, 0x2000).unwrap()),
            outputs: vec![SectionDesc::new(0x2000, 0x1000).unwrap()].into_boxed_slice(),
            bar2_size: SHARED_MEMORY_SIZE,
        };
        let error = layout.validate().unwrap_err();
        assert!(error.to_string().contains("overlaps"));
    }

    #[test]
    fn validate_accepts_a_common_section_between_outputs() {
        let layout = IvshmemMemoryLayout {
            state_table: SectionDesc::new(0, 0x1000).unwrap(),
            common: Some(SectionDesc::new(0x1000, 0x1000).unwrap()),
            outputs: vec![
                SectionDesc::new(0x2000, 0x1000).unwrap(),
                SectionDesc::new(0x3000, 0x1000).unwrap(),
            ]
            .into_boxed_slice(),
            bar2_size: SHARED_MEMORY_SIZE,
        };
        layout.validate().unwrap();
        assert_eq!(layout.classify(0x1000), Bar2Section::Common);
        assert_eq!(layout.classify(0x2000), Bar2Section::Output(peer(0)));
        assert_eq!(layout.classify(0x3000), Bar2Section::Output(peer(1)));
    }

    fn peer(value: u16) -> PeerId {
        PeerId::new(value)
    }
}
