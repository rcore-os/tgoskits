//! Per-peer direct-mapping plan of one ivshmem link.
//!
//! The plan is the only conversion point from the shared [`IvshmemMemoryLayout`]
//! to the mappings one peer needs in its stage-2: every section of the F5
//! layout becomes a [`DirectMapping`] with the peer's permission, and the
//! reserved tail is deliberately left unmapped so stray accesses fault
//! instead of silently succeeding.

use alloc::{format, string::String, vec::Vec};

use super::{
    backing::BackingAllocation,
    error::IvshmemError,
    layout::{Bar2Section, IvshmemMemoryLayout},
    link::PeerId,
};
use crate::stage2_remap::{DirectMapping, GpaRange};

/// Direct-mapping plan of one link as seen by one peer.
pub struct IvshmemDirectPlan {
    bar2_gpa: u64,
    bar2_size: u64,
    mappings: Vec<DirectMapping>,
}

impl IvshmemDirectPlan {
    /// Derives every section mapping for `peer`.
    ///
    /// Conversion rules, from `layout` + resolved BAR2 GPA + backing HPA:
    /// StateTable maps read-only; Common maps read/write; the peer's own
    /// Output maps read/write; other peers' Output sections map read-only;
    /// Reserved pages are not mapped at all, so accesses fault instead of
    /// silently succeeding.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidLayout`] when a section is misaligned
    /// or the BAR2 GPA is not page aligned.
    pub fn derive(
        layout: &IvshmemMemoryLayout,
        bar2_gpa: u64,
        backing: &BackingAllocation,
        peer: PeerId,
    ) -> Result<Self, IvshmemError> {
        const PAGE: u64 = 0x1000;
        let reject = |detail: String| IvshmemError::InvalidLayout { detail };
        if !bar2_gpa.is_multiple_of(PAGE) {
            return Err(reject(format!(
                "BAR2 GPA {bar2_gpa:#x} is not {PAGE:#x}-aligned"
            )));
        }
        let hpa = backing.hpa_base();
        if !hpa.is_multiple_of(PAGE) {
            return Err(reject(format!(
                "backing HPA {hpa:#x} is not {PAGE:#x}-aligned"
            )));
        }

        let mut mappings = Vec::new();
        mappings
            .try_reserve_exact(layout.outputs().len() + 2)
            .map_err(|_| IvshmemError::AllocationFailed {
                operation: "derive ivshmem direct-mapping plan",
            })?;
        let mut push = |section: Bar2Section, offset: u64, size: u64| -> Result<(), IvshmemError> {
            let writable = section.allows_write(peer);
            mappings.push(
                DirectMapping::new(
                    bar2_gpa + offset,
                    hpa + offset,
                    size,
                    writable,
                    section.name(),
                )
                .map_err(|error| IvshmemError::InvalidLayout {
                    detail: format!("{error}"),
                })?,
            );
            Ok(())
        };

        push(Bar2Section::StateTable, 0, layout.state_table().size())?;
        if let Some(common) = layout.common() {
            push(Bar2Section::Common, common.offset(), common.size())?;
        }
        for (index, output) in layout.outputs().iter().enumerate() {
            let owner = PeerId::new(index as u16);
            push(Bar2Section::Output(owner), output.offset(), output.size())?;
        }
        // Reserved pages stay unmapped by design.
        Ok(Self {
            bar2_gpa,
            bar2_size: layout.bar2_size(),
            mappings,
        })
    }

    /// Returns the resolved BAR2 GPA the plan was built from.
    pub const fn bar2_gpa(&self) -> u64 {
        self.bar2_gpa
    }

    /// Returns the mappings in ascending GPA order.
    pub fn mappings(&self) -> &[DirectMapping] {
        &self.mappings
    }

    /// Returns the GPA range covered by the whole BAR2, for revocation.
    pub const fn revocation_range(&self) -> GpaRange {
        GpaRange::new(self.bar2_gpa, self.bar2_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ivshmem::{SHARED_MEMORY_SIZE, link::LinkProfile};

    fn layout() -> IvshmemMemoryLayout {
        IvshmemMemoryLayout::derive(SHARED_MEMORY_SIZE, LinkProfile::baseline()).unwrap()
    }

    fn backing() -> BackingAllocation {
        // Test-only allocation shell: the plan reads the base address and
        // never dereferences the virtual pointer.
        BackingAllocation::from_parts(0x8_0000_0000, SHARED_MEMORY_SIZE, core::ptr::null_mut())
    }

    #[test]
    fn owner_and_visitor_views_differ_only_in_output_writability() {
        let layout = layout();
        let owner =
            IvshmemDirectPlan::derive(&layout, 0x0c00_0000, &backing(), PeerId::new(0)).unwrap();
        let visitor =
            IvshmemDirectPlan::derive(&layout, 0x0c00_0000, &backing(), PeerId::new(1)).unwrap();

        // Both views carry state table + two outputs; the reserved tail is
        // absent from both, and the section sequence is identical.
        assert_eq!(owner.mappings().len(), 3);
        assert_eq!(visitor.mappings().len(), 3);
        let owner_labels = owner
            .mappings()
            .iter()
            .map(|mapping| mapping.label())
            .collect::<Vec<_>>();
        let visitor_labels = visitor
            .mappings()
            .iter()
            .map(|mapping| mapping.label())
            .collect::<Vec<_>>();
        assert_eq!(owner_labels, visitor_labels);
        // The state table is read-only in every view.
        assert!(!owner.mappings()[0].writable());
        assert_eq!(owner.mappings()[0].gpa_base(), 0x0c00_0000);
        assert_eq!(owner.mappings()[0].hpa_base(), 0x8_0000_0000);
        // Peer 0's own output is writable, peer 1's is read-only.
        assert!(owner.mappings()[1].writable());
        assert!(!owner.mappings()[2].writable());
        // The visitor view flips the writability.
        assert!(!visitor.mappings()[1].writable());
        assert!(visitor.mappings()[2].writable());
    }

    #[test]
    fn mappings_cover_the_layout_sections_exactly() {
        let layout = layout();
        let plan =
            IvshmemDirectPlan::derive(&layout, 0x0c00_0000, &backing(), PeerId::new(0)).unwrap();
        let outputs = layout.outputs();
        assert_eq!(
            plan.mappings()[1].gpa_base(),
            0x0c00_0000 + outputs[0].offset()
        );
        assert_eq!(plan.mappings()[1].size(), outputs[0].size());
        assert_eq!(
            plan.mappings()[2].gpa_base(),
            0x0c00_0000 + outputs[1].offset()
        );
        assert_eq!(plan.mappings()[2].size(), outputs[1].size());
    }

    #[test]
    fn revocation_covers_the_whole_bar2() {
        let layout = layout();
        let plan =
            IvshmemDirectPlan::derive(&layout, 0x0c00_0000, &backing(), PeerId::new(1)).unwrap();
        assert_eq!(plan.bar2_gpa(), 0x0c00_0000);
        let range = plan.revocation_range();
        assert_eq!(range.base(), 0x0c00_0000);
        assert_eq!(range.size(), SHARED_MEMORY_SIZE);
    }

    #[test]
    fn rejects_misaligned_addresses() {
        let layout = layout();
        assert!(
            IvshmemDirectPlan::derive(&layout, 0x0c00_0800, &backing(), PeerId::new(0)).is_err()
        );
        let unaligned_backing =
            BackingAllocation::from_parts(0x8_0000_0800, SHARED_MEMORY_SIZE, core::ptr::null_mut());
        assert!(
            IvshmemDirectPlan::derive(&layout, 0x0c00_0000, &unaligned_backing, PeerId::new(0))
                .is_err()
        );
    }

    #[test]
    fn one_page_outputs_derive_without_reserved_mappings() {
        // A three-page BAR2 with one output page per peer hosts the state
        // table plus both outputs; nothing remains for the reserved tail.
        let profile = LinkProfile::new(2, 0, 0x1000).unwrap();
        let layout = IvshmemMemoryLayout::derive(0x3000, profile).unwrap();
        let plan =
            IvshmemDirectPlan::derive(&layout, 0x0c00_0000, &backing(), PeerId::new(0)).unwrap();
        assert_eq!(plan.mappings().len(), 3);
    }
}
