//! Production page-table retirement ordering, observed at capability boundaries.

#![cfg(not(target_os = "none"))]

mod retirement_support;

use page_table_generic::*;
use retirement_support::*;

#[test]
fn base_page_conflict_reports_existing_physical_address() {
    assert_conflict_preserves_mapping(4096);
}

#[test]
fn huge_page_conflict_reports_existing_physical_address() {
    assert_conflict_preserves_mapping(0x20_0000);
}

#[test]
fn single_leaf_unmap_retains_intermediate_tables_for_remote_walkers() {
    let mut table =
        PageTable::<RetirementMeta, RetirementAllocator>::new(RetirementAllocator).unwrap();
    table
        .map_page(0x20_0000.into(), 0x40_0000.into(), 4096, 1)
        .unwrap();
    EVENTS.with_borrow_mut(Vec::clear);

    table.unmap_page(0x20_0000.into()).unwrap();

    assert_eq!(table.query(0x20_0000.into()), Err(PagingError::NotMapped));
    EVENTS.with_borrow(|events| {
        assert!(events.iter().any(|event| matches!(event, Event::Flush)));
        assert!(!events.iter().any(|event| matches!(event, Event::Free)));
    });
}

#[test]
fn deferred_range_retains_tables_and_reports_an_error_prefix() {
    for partial_huge in [false, true] {
        let mut table = new_table();
        let start = VirtAddr::from(0x20_0000);
        let huge = VirtAddr::from(0x40_0000);
        for offset in [0, 4096, 0x10_0000] {
            table
                .map_page(start + offset, PhysAddr::from(0x80_0000 + offset), 4096, 1)
                .unwrap();
        }
        if partial_huge {
            table
                .map_page(huge, PhysAddr::from(0xa0_0000), 0x20_0000, 1)
                .unwrap();
        }
        reset_events();
        let mut batches = Vec::new();
        let end = if partial_huge { huge + 4096 } else { huge };
        let result = table.unmap_range_deferred(start..end, |batch| batches.push(batch));
        assert_eq!(result.is_err(), partial_huge);
        if !partial_huge {
            assert_eq!(result.unwrap(), 3);
        }
        for offset in [0, 4096, 0x10_0000] {
            assert_eq!(table.query(start + offset), Err(PagingError::NotMapped));
        }
        if partial_huge {
            assert_eq!(table.query(huge).unwrap().2, 0x20_0000);
        }
        EVENTS.with_borrow(|events| {
            assert!(events.iter().any(|event| matches!(event, Event::Clear)));
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, Event::Free | Event::Flush | Event::Batch(_)))
            );
        });
        assert!(
            batches
                .iter()
                .map(DeferredPageTableFrames::len)
                .sum::<usize>()
                > 0
        );
        RetirementMeta::flush(None);
        for batch in batches {
            // SAFETY: the recording metadata's completed flush revokes all
            // users of this exclusively owned test table before reclamation.
            unsafe { batch.reclaim() };
        }
        EVENTS
            .with_borrow(|events| assert!(events.iter().any(|event| matches!(event, Event::Free))));
        reset_events();
        assert_eq!(
            table.unmap_range_deferred(start..huge, |_| {
                panic!("a range containing only holes must not report a PTE change");
            }),
            Ok(0)
        );
        EVENTS.with_borrow(|events| assert!(events.is_empty()));
    }
}

#[test]
fn occupied_query_retains_inaccessible_mapping_for_unmap() {
    for size in [4096, 0x20_0000] {
        let mut table = new_table();
        let address = VirtAddr::from(0x20_0000);
        let physical = PhysAddr::from(0x80_0000);
        assert!(matches!(
            table.query_occupied(address),
            Err(PagingError::NotMapped)
        ));
        table.map_page(address, physical, size, 1).unwrap();
        table.protect_page(address, 0).unwrap();
        assert_eq!(table.query(address), Err(PagingError::NotMapped));
        reset_events();
        let (pte, level) = table.query_occupied(address + size - 1).unwrap();
        assert_eq!(level, if size == 4096 { 1 } else { 2 });
        assert_eq!(pte.paddr(level > 1), physical);
        assert_eq!(pte.config(level > 1), 0);
        EVENTS.with_borrow(|events| assert!(events.is_empty()));
        assert_eq!(
            table.unmap_page(address + size - 1).unwrap(),
            (physical, 0, size)
        );
        assert!(matches!(
            table.query_occupied(address),
            Err(PagingError::NotMapped)
        ));
    }
}

#[test]
fn changed_frame_remap_invalidates_absent_leaf_before_make() {
    for size in [4096, 0x20_0000] {
        let mut table = new_table();
        let address = VirtAddr::from(0x20_0000);
        table.map_page(address, 0x80_0000.into(), size, 1).unwrap();
        let _observation = LeafObservation::new(&table, address);

        assert_eq!(table.remap_page(address, 0xa0_0000.into(), 1), Ok(size));

        FLUSHED_LEAVES.with_borrow(|leaves| {
            assert!(
                leaves.len() >= 2 && leaves[0] == 0,
                "replacement became visible before invalidating the absent old leaf: {leaves:x?}"
            );
            assert_eq!(
                leaves.last().copied(),
                Some(0xa0_0001 | if size > 4096 { 2 } else { 0 }),
                "the final completion must observe the replacement descriptor"
            );
        });
        assert_eq!(table.query(address).unwrap().0, PhysAddr::from(0xa0_0000));
    }
}

#[test]
fn same_frame_remap_does_not_break_the_leaf() {
    let mut table = new_table();
    let address = VirtAddr::from(0x20_0000);
    table.map_page(address, 0x80_0000.into(), 4096, 1).unwrap();
    let _observation = LeafObservation::new(&table, address);
    assert_eq!(table.remap_page(address, 0x80_0000.into(), 0), Ok(4096));
    FLUSHED_LEAVES.with_borrow(|leaves| assert_eq!(leaves.as_slice(), &[0x80_0000]));
    // The retained descriptor is intentionally non-present; query translates
    // only present mappings, unlike the occupied-leaf ownership walker.
    assert_eq!(table.query(address), Err(PagingError::NotMapped));
}

#[test]
fn protect_completes_descriptor_publication_before_cow_sharing() {
    let mut table = new_table();
    let address = VirtAddr::from(0x20_0000);
    table.map_page(address, 0x80_0000.into(), 4096, 1).unwrap();
    let _observation = LeafObservation::new(&table, address);
    reset_events();
    assert_eq!(table.protect_page(address, 0), Ok(4096));
    FLUSHED_LEAVES.with_borrow(|leaves| assert_eq!(leaves.as_slice(), &[0x80_0000]));
    EVENTS.with_borrow(|events| {
        assert!(
            events.iter().any(|event| matches!(event, Event::Batch(1))),
            "permission downgrade lacks descriptor-publication completion: {events:?}"
        );
    });
}

#[test]
fn dense_deferred_unmap_walks_each_table_once() {
    let mut table = new_table();
    let start = VirtAddr::from(0x20_0000);
    let pages = 1024;
    for index in 0..pages {
        table
            .map_page(
                start + index * 4096,
                (0x80_0000 + index * 4096).into(),
                4096,
                1,
            )
            .unwrap();
    }
    reset_events();
    let mut batches = Vec::new();

    assert_eq!(
        table.unmap_range_deferred(start..start + pages * 4096, |batch| batches.push(batch)),
        Ok(pages)
    );
    assert!(
        WALKS.get() <= 12,
        "walk restarted per page: {}",
        WALKS.get()
    );
    assert!(root_entries(&table).iter().all(PageTableEntry::unused));
    EVENTS.with_borrow(|events| {
        assert!(events.iter().any(|event| matches!(event, Event::Clear)));
        assert!(!events.iter().any(|event| matches!(event, Event::Free)));
    });
    assert!(batches.iter().any(|batch| !batch.is_empty()));
    RetirementMeta::flush(None);
    for batch in batches {
        // SAFETY: this test table is never installed in hardware and the
        // completed flush confirms all old walks have stopped before reuse.
        unsafe { batch.reclaim() };
    }
}

#[test]
fn deferred_leaf_flush_still_invalidates_retired_tables() {
    let mut table = new_table();
    table
        .map_page(0x20_0000.into(), 0x80_0000.into(), 4096, 1)
        .unwrap();
    reset_events();
    table
        .unmap_with_config(&UnmapConfig {
            start_vaddr: 0x20_0000.into(),
            size: 4096,
            flush: false,
        })
        .unwrap();
    assert_retirement_order();
    EVENTS.with_borrow(|events| {
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Free))
                .count(),
            3
        )
    });
}

fn assert_conflict_preserves_mapping(page_size: usize) {
    let mut table = new_table();
    let vaddr = VirtAddr::from(0x20_0000);
    let existing_paddr = PhysAddr::from(0x40_0000);
    let replacement = PhysAddr::from(0x60_0000);
    table.map_page(vaddr, existing_paddr, page_size, 1).unwrap();
    reset_events();

    let result = table.map_page(vaddr, replacement, page_size, 0);
    assert_eq!(table.query(vaddr).unwrap(), (existing_paddr, 1, page_size));
    EVENTS.with_borrow(|events| {
        assert!(
            events.is_empty(),
            "conflict must not mutate or retire the occupied mapping: {events:?}"
        );
    });
    assert_eq!(
        result,
        Err(PagingError::MappingConflict {
            vaddr,
            existing_paddr,
        }),
    );
}

#[test]
fn deferred_unmap_retains_preallocated_shared_root_directories() {
    let mut table = new_table();
    let start = VirtAddr::from(0x20_0000);
    table.preallocate_shared_root_entries(start, 4096).unwrap();
    let root_entry = root_entries(&table)[0];
    table.map_page(start, 0x80_0000.into(), 4096, 1).unwrap();
    let mut batches = Vec::new();
    assert_eq!(
        table.unmap_range_deferred(start..start + 4096, |batch| batches.push(batch)),
        Ok(1)
    );
    let retained = root_entries(&table)[0];
    assert!(
        !retained.unused(),
        "borrowed roots still need this directory"
    );
    assert_eq!(retained.paddr(true), root_entry.paddr(true));
    RetirementMeta::flush(None);
    for batch in batches {
        // SAFETY: the test table is never installed in hardware, and the
        // completed flush precedes reclamation.
        unsafe { batch.reclaim() };
    }
    table.map_page(start, 0x90_0000.into(), 4096, 1).unwrap();
    assert_eq!(root_entries(&table)[0].paddr(true), root_entry.paddr(true));
    assert_eq!(table.query(start).unwrap().0, PhysAddr::from(0x90_0000));
}
