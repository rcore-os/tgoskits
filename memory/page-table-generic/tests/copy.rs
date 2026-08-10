#![cfg(all(not(target_os = "none"), feature = "copy-from"))]

mod mocks;

use mocks::*;
use page_table_generic::*;

const PAGE_SIZE: usize = 0x1000;
const ROOT_ENTRY_SIZE: usize = 1 << 39;
const KERNEL_SPACE_BASE: usize = 0xffff_8000_0000_0000;
const KERNEL_IMAGE_BASE: usize = 0xffff_ffff_8000_0000;

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
