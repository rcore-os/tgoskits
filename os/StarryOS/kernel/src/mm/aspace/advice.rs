//! Publication of inheritance policy at materialized huge-page boundaries.

use super::*;

impl AddrSpace {
    /// A wiped fragment must not share a leaf with a normally inherited
    /// neighbour: fork would otherwise copy the wiped bytes with that leaf.
    /// Use the existing split/receipt protocol, keeping the parent contents
    /// and permissions unchanged. Future faults already respect VMA bounds.
    pub(super) fn publish_wipe_on_fork(
        &mut self,
        previous: Arc<VmaMap>,
        successor: VmaMap,
        range: VirtAddrRange,
    ) -> StarryResult {
        let mut mutation = self.prepare_mutation();
        mutation
            .try_reserve_tlb_ranges(2)
            .map_err(|_| StarryError::NoMemory)?;
        mutation.set_vma_delta(VmaDelta {
            split: u32::try_from(successor.len().saturating_sub(previous.len()))
                .unwrap_or(u32::MAX),
            merged: u32::try_from(previous.len().saturating_sub(successor.len()))
                .unwrap_or(u32::MAX),
            ..VmaDelta::default()
        });
        let splits = self.apply_partial_huge_splits(range)?;
        if splits.is_empty() {
            return self.publish_vma_metadata_successor(previous, successor, "VMA fork policy");
        }
        for split in &splits {
            let Some(range) =
                TlbRange::new(split.installed.block_vaddr(), split.installed.block_size())
            else {
                return self.abort_unpublished_huge_splits(splits, StarryError::BadState);
            };
            mutation.add_tlb_range(range);
        }
        mutation.set_mapping_delta(MappingDelta {
            attached: u32::try_from(
                splits
                    .iter()
                    .map(|split| split.child_slots.len().saturating_sub(1))
                    .sum::<usize>(),
            )
            .unwrap_or(u32::MAX),
            ..MappingDelta::default()
        });
        self.vma_root = Arc::new(successor);
        match self.commit_mutation_classified(mutation) {
            Ok(()) => Ok(()),
            Err(CommitMutationError::PublishedPendingTlb(error)) => Err(error),
            Err(CommitMutationError::Unpublished(error)) => {
                self.vma_root = previous;
                self.abort_unpublished_huge_splits(splits, error)
            }
        }
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use ax_memory_addr::PAGE_SIZE_2M;

    use super::*;

    #[axtest::axtest]
    fn wiped_fork_splits_huge_leaf_and_rolls_back_unpublished_policy() {
        let start = VirtAddr::from_usize(0x7200_0000);
        let middle = start + PAGE_SIZE_4K;
        let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
        let mut parent = AddrSpace::new_empty(start, PAGE_SIZE_2M).unwrap();
        parent
            .map(
                start,
                PAGE_SIZE_2M,
                flags,
                false,
                MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[wipe-fork]"),
            )
            .unwrap();
        parent
            .advise_huge_pages(start, PAGE_SIZE_2M, HugePageAdvice::Prefer)
            .unwrap();
        assert_eq!(
            parent.handle_page_fault_result(start, PageFaultFlags::WRITE | PageFaultFlags::USER),
            FaultResult::Handled
        );
        let original = parent.pt.query(start).unwrap();
        assert_eq!(original.2, PAGE_SIZE_2M);
        let old_root = parent.vma_root.clone();
        parent.mutation_gate.fail_next_commit_before_publish();
        assert!(
            parent
                .advise_vma_policy(middle, PAGE_SIZE_4K, VmaAdviceUpdate::WipeOnFork(true))
                .is_err()
        );
        assert!(Arc::ptr_eq(&parent.vma_root, &old_root));
        assert_eq!(parent.pt.query(start).unwrap(), original);
        assert_eq!(parent.mapping_slots.len(), 1);
        assert!(!parent.mutation_gate.needs_repair());

        parent
            .advise_vma_policy(middle, PAGE_SIZE_4K, VmaAdviceUpdate::WipeOnFork(true))
            .unwrap();
        assert_eq!(parent.pt.query(middle).unwrap().2, PAGE_SIZE_4K);
        let child = parent.try_clone().unwrap();
        let mut child = child.lock();
        assert!(matches!(
            child.pt.query(middle),
            Err(PagingError::NotMapped)
        ));
        assert_eq!(child.pt.query(start).unwrap().0, original.0);
        assert_eq!(parent.pt.query(middle).unwrap().1, flags);
        assert_eq!(
            child.handle_page_fault_result(middle, PageFaultFlags::WRITE | PageFaultFlags::USER),
            FaultResult::Handled
        );
        assert_ne!(
            child.pt.query(middle).unwrap().0,
            parent.pt.query(middle).unwrap().0
        );
        assert!(
            child
                .find_area_snapshot(middle)
                .unwrap()
                .advice_policy
                .wipe_on_fork()
        );
        child.reset_uninstalled_for_loader().unwrap();
        parent.reset_uninstalled_for_loader().unwrap();
    }
}
