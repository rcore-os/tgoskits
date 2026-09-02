#![cfg(all(not(target_os = "none"), feature = "copy-from"))]

mod mocks;

use mocks::*;
use page_table_generic::*;

const PAGE_SIZE: usize = 0x1000;
const ROOT_ENTRY_SIZE: usize = 1 << 39;
const SV39_ROOT_BLOCK_SIZE: usize = 1 << 30;
const KERNEL_SPACE_BASE: usize = 0xffff_8000_0000_0000;
const KERNEL_IMAGE_BASE: usize = 0xffff_ffff_8000_0000;

#[derive(Debug, Clone, Copy)]
struct T4kL3RootBlocks;

impl TableMeta for T4kL3RootBlocks {
    type P = PteImpl;

    const PAGE_SIZE: usize = PAGE_SIZE;
    const LEVEL_BITS: &[usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_vaddr: Option<VirtAddr>) {}
}

fn map_page<A: FrameAllocator>(page_table: &mut PageTable<T4kL4, A>, vaddr: usize, paddr: usize) {
    page_table
        .map_page(
            VirtAddr::from_usize(vaddr),
            PhysAddr::from_usize(paddr),
            0x1000,
            (MappingFlags::READ | MappingFlags::WRITE).into(),
        )
        .unwrap();
}

#[test]
fn copies_canonical_high_root_entries() {
    const KERNEL_BASE: usize = 0xffff_8000_0000_0000;
    const KERNEL_PAGE: usize = KERNEL_BASE + 0x20_0000;
    const PHYS_PAGE: usize = 0x40_0000;

    let mut source = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();
    let mut target = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();
    map_page(&mut source, KERNEL_PAGE, PHYS_PAGE);

    unsafe {
        target.share_root_entries_from(&source, VirtAddr::from_usize(KERNEL_BASE), ROOT_ENTRY_SIZE)
    }
    .unwrap();

    assert_eq!(
        target.translate_phys(VirtAddr::from_usize(KERNEL_PAGE)),
        Ok(PhysAddr::from_usize(PHYS_PAGE))
    );
}

#[test]
fn copied_root_entries_remain_shared_and_borrowed() {
    const FIRST_PAGE: usize = 0x20_0000;
    const SECOND_PAGE: usize = FIRST_PAGE + PAGE_SIZE;

    let allocator = TrackedFram4k::new();
    let mut source = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    map_page(&mut source, FIRST_PAGE, 0x40_0000);
    let source_frames = allocator.allocated_count();

    {
        let mut target = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
        unsafe {
            target.share_root_entries_from(&source, VirtAddr::from_usize(0), ROOT_ENTRY_SIZE)
        }
        .unwrap();

        assert_eq!(allocator.allocated_count(), source_frames + 1);

        map_page(&mut source, SECOND_PAGE, 0x80_0000);
        assert_eq!(
            target.translate_phys(VirtAddr::from_usize(SECOND_PAGE)),
            Ok(PhysAddr::from_usize(0x80_0000))
        );
    }

    assert_eq!(allocator.allocated_count(), source_frames);
    assert_eq!(
        source.translate_phys(VirtAddr::from_usize(SECOND_PAGE)),
        Ok(PhysAddr::from_usize(0x80_0000))
    );
}

#[test]
fn detaching_borrower_reclaims_only_its_owned_root() {
    const FIRST_PAGE: usize = 0x20_0000;
    const SECOND_PAGE: usize = FIRST_PAGE + PAGE_SIZE;

    let allocator = TrackedFram4k::new();
    let mut source = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    map_page(&mut source, FIRST_PAGE, 0x40_0000);
    let source_frames = allocator.allocated_count();

    let mut target = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    unsafe { target.share_root_entries_from(&source, VirtAddr::from_usize(0), ROOT_ENTRY_SIZE) }
        .unwrap();
    assert_eq!(allocator.allocated_count(), source_frames + 1);

    let mut detached = Vec::new();
    // SAFETY: this test owns `target` exclusively and never installs its root
    // in hardware. Reclaiming the returned tokens is therefore immediately
    // safe and must not touch the root entries borrowed from `source`.
    unsafe {
        target.detach(|token| detached.push(token));
    }
    assert_eq!(detached.len(), 1, "only the target root is owned");
    for token in detached {
        token.reclaim();
    }
    assert_eq!(allocator.allocated_count(), source_frames);

    map_page(&mut source, SECOND_PAGE, 0x80_0000);
    assert_eq!(
        source.translate_phys(VirtAddr::from_usize(SECOND_PAGE)),
        Ok(PhysAddr::from_usize(0x80_0000))
    );
}

#[test]
fn preallocated_root_entry_survives_empty_unmap_and_later_publication() {
    const FIRST_PAGE: usize = 0x20_0000;
    const SECOND_PAGE: usize = 0x40_0000;

    let allocator = TrackedFram4k::new();
    let mut source = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    source
        .preallocate_shared_root_entries(VirtAddr::from_usize(0), ROOT_ENTRY_SIZE)
        .unwrap();

    let mut target = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    unsafe { target.share_root_entries_from(&source, VirtAddr::from_usize(0), ROOT_ENTRY_SIZE) }
        .unwrap();

    map_page(&mut source, FIRST_PAGE, 0x40_0000);
    assert_eq!(
        target.translate_phys(VirtAddr::from_usize(FIRST_PAGE)),
        Ok(PhysAddr::from_usize(0x40_0000))
    );

    source.unmap_page(VirtAddr::from_usize(FIRST_PAGE)).unwrap();
    map_page(&mut source, SECOND_PAGE, 0x80_0000);
    assert_eq!(
        target.translate_phys(VirtAddr::from_usize(SECOND_PAGE)),
        Ok(PhysAddr::from_usize(0x80_0000))
    );
}

#[test]
fn preallocated_root_entry_splits_existing_root_block() {
    const REPLACED_PAGE: usize = 0x20_0000;
    const ROOT_BLOCK_PADDR: usize = 0x4000_0000;
    const REPLACEMENT_PADDR: usize = 0x9000_0000;

    let allocator = TrackedFram4k::new();
    let mut source = PageTable::<T4kL3RootBlocks, TrackedFram4k>::new(allocator).unwrap();
    source
        .map_linear_pages(
            VirtAddr::from_usize(0),
            PhysAddr::from_usize(ROOT_BLOCK_PADDR),
            SV39_ROOT_BLOCK_SIZE,
            (MappingFlags::READ | MappingFlags::WRITE).into(),
            true,
        )
        .unwrap();
    assert_eq!(
        source.query(VirtAddr::from_usize(REPLACED_PAGE)).unwrap().2,
        SV39_ROOT_BLOCK_SIZE
    );

    source
        .preallocate_shared_root_entries(VirtAddr::from_usize(0), SV39_ROOT_BLOCK_SIZE)
        .unwrap();
    let (split_paddr, split_config, split_size) =
        source.query(VirtAddr::from_usize(REPLACED_PAGE)).unwrap();
    assert_eq!(
        split_paddr,
        PhysAddr::from_usize(ROOT_BLOCK_PADDR + REPLACED_PAGE)
    );
    assert_eq!(split_config, MappingFlags::READ | MappingFlags::WRITE);
    assert_eq!(split_size, 0x20_0000);

    let mut target = PageTable::<T4kL3RootBlocks, TrackedFram4k>::new(allocator).unwrap();
    unsafe {
        target.share_root_entries_from(&source, VirtAddr::from_usize(0), SV39_ROOT_BLOCK_SIZE)
    }
    .unwrap();

    source
        .unmap_page(VirtAddr::from_usize(REPLACED_PAGE))
        .unwrap();
    source
        .map_page(
            VirtAddr::from_usize(REPLACED_PAGE),
            PhysAddr::from_usize(REPLACEMENT_PADDR),
            PAGE_SIZE,
            (MappingFlags::READ | MappingFlags::WRITE).into(),
        )
        .unwrap();
    assert_eq!(
        target.translate_phys(VirtAddr::from_usize(REPLACED_PAGE)),
        Ok(PhysAddr::from_usize(REPLACEMENT_PADDR))
    );
}

#[test]
fn cloned_missing_root_entries_preserve_boot_only_kernel_mappings() {
    const DIRECT_PAGE: usize = KERNEL_SPACE_BASE + 0x20_0000;
    const KERNEL_IMAGE_PAGE: usize = KERNEL_IMAGE_BASE + 0x40_0000;

    let allocator = TrackedFram4k::new();
    let mut boot = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    map_page(&mut boot, DIRECT_PAGE, 0x20_0000);
    map_page(&mut boot, KERNEL_IMAGE_PAGE, 0x40_0000);

    let mut managed = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    map_page(&mut managed, DIRECT_PAGE, 0x20_0000);
    managed
        .clone_missing_root_entries_from(
            &boot,
            VirtAddr::from_usize(KERNEL_SPACE_BASE),
            usize::MAX - KERNEL_SPACE_BASE,
        )
        .unwrap();
    drop(boot);

    let mut user = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    unsafe {
        user.share_root_entries_from(
            &managed,
            VirtAddr::from_usize(KERNEL_SPACE_BASE),
            usize::MAX - KERNEL_SPACE_BASE,
        )
    }
    .unwrap();

    assert_eq!(
        user.translate_phys(VirtAddr::from_usize(KERNEL_IMAGE_PAGE)),
        Ok(PhysAddr::from_usize(0x40_0000))
    );
}

#[test]
fn cloned_root_entries_preserve_non_present_leaves() {
    const PAGE: usize = 0x20_0000;
    const PHYS_PAGE: usize = 0x40_0000;

    let allocator = TrackedFram4k::new();
    let mut source = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    source
        .map_page(
            VirtAddr::from_usize(PAGE),
            PhysAddr::from_usize(PHYS_PAGE),
            0x1000,
            MappingFlags::empty().into(),
        )
        .unwrap();

    let mut target = PageTable::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
    target
        .clone_missing_root_entries_from(&source, VirtAddr::from_usize(0), ROOT_ENTRY_SIZE)
        .unwrap();
    target
        .protect_page(VirtAddr::from_usize(PAGE), MappingFlags::READ.into())
        .unwrap();

    assert_eq!(
        target.query(VirtAddr::from_usize(PAGE)).unwrap().0,
        PhysAddr::from_usize(PHYS_PAGE)
    );
}
