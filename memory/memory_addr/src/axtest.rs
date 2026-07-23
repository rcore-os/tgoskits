use axtest::prelude::*;

use crate::{
    AddrRange, DynPageIter, MemoryAddr, PAGE_SIZE_2M, PAGE_SIZE_4K, PageIter, PhysAddr,
    PhysAddrRange, VirtAddr, VirtAddrRange, addr_range, pa_range, va_range,
};

#[axtest]
fn memory_addr_alignment_helpers_cover_generic_and_4k_rules() {
    ax_assert_eq!(crate::align_down(0x12345, 0x1000), 0x12000);
    ax_assert_eq!(crate::align_up(0x12345, 0x1000), 0x13000);
    ax_assert_eq!(crate::align_offset(0x12345, 0x1000), 0x345);
    ax_assert!(crate::is_aligned(0x12000, 0x1000));
    ax_assert!(!crate::is_aligned(0x12345, 0x1000));

    let vaddr = VirtAddr::from(0x12345);
    ax_assert_eq!(vaddr.align_down(0x1000usize), VirtAddr::from(0x12000));
    ax_assert_eq!(vaddr.align_up_4k(), VirtAddr::from(0x13000));
    ax_assert_eq!(vaddr.align_offset_4k(), 0x345);
    ax_assert!(!vaddr.is_aligned_4k());
    ax_assert!(VirtAddr::from(0x12000).is_aligned(PAGE_SIZE_4K));
}

#[axtest]
fn memory_addr_arithmetic_reports_wrapping_checked_and_distance_results() {
    let base = PhysAddr::from(0x4000);
    ax_assert_eq!(base.offset(0x80), PhysAddr::from(0x4080));
    ax_assert_eq!(base.offset(-0x100), PhysAddr::from(0x3f00));
    ax_assert_eq!(base.add(0x2000), PhysAddr::from(0x6000));
    ax_assert_eq!(base.sub(0x1000), PhysAddr::from(0x3000));
    ax_assert_eq!(base.offset_from(PhysAddr::from(0x3000)), 0x1000);
    ax_assert_eq!(base.sub_addr(PhysAddr::from(0x1000)), 0x3000);

    let (wrapped_add, add_overflow) = PhysAddr::from(usize::MAX).overflowing_add(1);
    ax_assert_eq!(wrapped_add, PhysAddr::from(0));
    ax_assert!(add_overflow);
    ax_assert_eq!(PhysAddr::from(usize::MAX).checked_add(1), None);

    let (wrapped_sub, sub_overflow) = PhysAddr::from(0).overflowing_sub(1);
    ax_assert_eq!(wrapped_sub, PhysAddr::from(usize::MAX));
    ax_assert!(sub_overflow);
    ax_assert_eq!(PhysAddr::from(0).checked_sub(1), None);
    ax_assert_eq!(
        PhysAddr::from(0x20).wrapping_offset(-0x40),
        PhysAddr::from(usize::MAX - 0x1f)
    );
    ax_assert_eq!(
        PhysAddr::from(usize::MAX).wrapping_add(2),
        PhysAddr::from(1)
    );
    ax_assert_eq!(
        PhysAddr::from(0).wrapping_sub(2),
        PhysAddr::from(usize::MAX - 1)
    );
    ax_assert_eq!(
        PhysAddr::from(0x10).wrapping_sub_addr(PhysAddr::from(0x20)),
        usize::MAX - 0xf
    );
    ax_assert_eq!(
        PhysAddr::from(0x10).overflowing_sub_addr(PhysAddr::from(0x20)),
        (usize::MAX - 0xf, true)
    );
    ax_assert_eq!(
        PhysAddr::from(0x20).overflowing_sub_addr(PhysAddr::from(0x10)),
        (0x10, false)
    );
    ax_assert_eq!(
        PhysAddr::from(0x10).checked_sub_addr(PhysAddr::from(0x20)),
        None
    );
    ax_assert_eq!(
        PhysAddr::from(0x20).checked_sub_addr(PhysAddr::from(0x10)),
        Some(0x10)
    );
}

#[axtest]
fn memory_addr_newtypes_format_and_pointer_helpers_hold() {
    let pa = PhysAddr::from_usize(0x1abc);
    ax_assert_eq!(pa.as_usize(), 0x1abc);
    ax_assert_eq!(alloc::format!("{pa:?}"), "PA:0x1abc");
    ax_assert_eq!(alloc::format!("{pa:x}"), "PA:0x1abc");
    ax_assert_eq!(alloc::format!("{pa:X}"), "PA:0x1ABC");

    let va = VirtAddr::from_usize(0x2abc);
    ax_assert_eq!(va.as_usize(), 0x2abc);
    ax_assert_eq!(alloc::format!("{va:?}"), "VA:0x2abc");
    ax_assert_eq!(alloc::format!("{va:x}"), "VA:0x2abc");
    ax_assert_eq!(alloc::format!("{va:X}"), "VA:0x2ABC");

    let value = 42_u64;
    let ptr = &value as *const u64;
    let addr = VirtAddr::from_ptr_of(ptr);
    ax_assert_eq!(addr.as_ptr_of::<u64>(), ptr);
    ax_assert_eq!(addr.as_ptr(), ptr.cast::<u8>());

    let mut value = 7_u32;
    let ptr = &mut value as *mut u32;
    let addr = VirtAddr::from_mut_ptr_of(ptr);
    ax_assert_eq!(addr.as_mut_ptr_of::<u32>(), ptr);
    ax_assert_eq!(addr.as_mut_ptr(), ptr.cast::<u8>());
}

#[axtest]
fn memory_addr_ranges_cover_contains_overlap_and_formatting_rules() {
    let range = va_range!(0x1000..0x3000);
    ax_assert!(!range.is_empty());
    ax_assert_eq!(range.size(), 0x2000);
    ax_assert!(range.contains(VirtAddr::from(0x1000)));
    ax_assert!(range.contains(VirtAddr::from(0x2fff)));
    ax_assert!(!range.contains(VirtAddr::from(0x3000)));

    ax_assert!(range.contains_range(va_range!(0x1800..0x2000)));
    ax_assert!(!range.contains_range(va_range!(0x0800..0x2000)));
    ax_assert!(va_range!(0x1800..0x2000).contained_in(range));
    ax_assert!(range.overlaps(va_range!(0x0800..0x1001)));
    ax_assert!(range.overlaps(va_range!(0x2fff..0x4000)));
    ax_assert!(!range.overlaps(va_range!(0x3000..0x4000)));

    let sized = VirtAddrRange::from_start_size(VirtAddr::from(0x5000), 0x800);
    ax_assert_eq!(sized.start, VirtAddr::from(0x5000));
    ax_assert_eq!(sized.end, VirtAddr::from(0x5800));
    ax_assert_eq!(
        VirtAddrRange::try_from_start_size(VirtAddr::from(usize::MAX), 2),
        None
    );
    ax_assert!(AddrRange::<usize>::try_new(0x3000, 0x1000).is_none());
    ax_assert!(AddrRange::<usize>::try_from(0x1000usize..0x2000).is_ok());

    let default_range: PhysAddrRange = Default::default();
    ax_assert!(default_range.is_empty());
    ax_assert_eq!(pa_range!(0x1000..0x2000).size(), 0x1000);
    let usize_range: AddrRange<usize> = addr_range!(0x10usize..0x20);
    ax_assert_eq!(usize_range.size(), 0x10);
}

#[axtest]
fn memory_addr_range_format_unchecked_and_boundary_rules_hold() {
    let range = va_range!(0xfec000..0xfff000usize);
    ax_assert_eq!(alloc::format!("{range:?}"), "VA:0xfec000..VA:0xfff000");
    ax_assert_eq!(alloc::format!("{range:x}"), "VA:0xfec000..VA:0xfff000");
    ax_assert_eq!(alloc::format!("{range:X}"), "VA:0xFEC000..VA:0xFFF000");

    let unchecked = unsafe { VirtAddrRange::new_unchecked(0x1000.into(), 0x1000.into()) };
    ax_assert!(unchecked.is_empty());
    ax_assert_eq!(unchecked.size(), 0);

    let sized = unsafe { VirtAddrRange::from_start_size_unchecked(usize::MAX.into(), 2) };
    ax_assert_eq!(sized.start, VirtAddr::from(usize::MAX));
    ax_assert_eq!(sized.end, VirtAddr::from(1));

    let range = va_range!(0x1000..0x2000);
    ax_assert!(!range.contains_range(va_range!(0x0fff..0x1fff)));
    ax_assert!(!range.contains_range(va_range!(0x1001..0x2001)));
    ax_assert!(range.contains_range(va_range!(0x1000..0x2000)));
    ax_assert!(range.contained_in(va_range!(0x0fff..0x2001)));
    ax_assert!(!range.overlaps(va_range!(0x0800..0x1000)));
    ax_assert!(!range.overlaps(va_range!(0x2000..0x2800)));
    ax_assert!(range.overlaps(va_range!(0x0fff..0x1001)));
    ax_assert!(range.overlaps(va_range!(0x1fff..0x2001)));

    let converted: VirtAddrRange = (VirtAddr::from(0x10)..VirtAddr::from(0x20))
        .try_into()
        .unwrap();
    ax_assert_eq!(converted, va_range!(0x10..0x20));
    ax_assert!(VirtAddrRange::try_from(VirtAddr::from(0x20)..VirtAddr::from(0x10)).is_err());
}

#[axtest]
fn memory_addr_page_iterators_accept_only_aligned_power_of_two_pages() {
    let pages = PageIter::<PAGE_SIZE_4K, usize>::new(0x1000, 0x4000)
        .unwrap()
        .collect::<alloc::vec::Vec<_>>();
    ax_assert_eq!(pages, alloc::vec![0x1000, 0x2000, 0x3000]);

    ax_assert!(PageIter::<PAGE_SIZE_4K, usize>::new(0x1001, 0x4000).is_none());
    ax_assert!(PageIter::<3, usize>::new(0x1000, 0x4000).is_none());

    let huge_pages = PageIter::<PAGE_SIZE_2M, PhysAddr>::new(
        PhysAddr::from(PAGE_SIZE_2M),
        PhysAddr::from(PAGE_SIZE_2M * 3),
    )
    .unwrap()
    .collect::<alloc::vec::Vec<_>>();
    ax_assert_eq!(
        huge_pages,
        alloc::vec![
            PhysAddr::from(PAGE_SIZE_2M),
            PhysAddr::from(PAGE_SIZE_2M * 2)
        ]
    );

    let dynamic = DynPageIter::<usize>::new(0, 0x3000, PAGE_SIZE_4K)
        .unwrap()
        .collect::<alloc::vec::Vec<_>>();
    ax_assert_eq!(dynamic, alloc::vec![0, 0x1000, 0x2000]);
    ax_assert!(DynPageIter::<usize>::new(0, 0x3001, PAGE_SIZE_4K).is_none());
    ax_assert!(DynPageIter::<usize>::new(0, 0x3000, 0x1800).is_none());
}

#[axtest]
fn memory_addr_overflowing_and_checked_ops_hold() {
    use crate::{PhysAddr, VirtAddr};

    // overflowing_sub
    let (result, overflow) = PhysAddr::from(0x100).overflowing_sub(0x50);
    ax_assert_eq!(result, PhysAddr::from(0xb0));
    ax_assert!(!overflow);
    let (result, overflow) = PhysAddr::from(0x50).overflowing_sub(0x100);
    ax_assert_eq!(result, PhysAddr::from(usize::MAX - 0xaf));
    ax_assert!(overflow);

    // checked_sub
    ax_assert_eq!(PhysAddr::from(0x100).checked_sub(0x50), Some(PhysAddr::from(0xb0)));
    ax_assert_eq!(PhysAddr::from(0x50).checked_sub(0x100), None);

    // sub_addr
    ax_assert_eq!(PhysAddr::from(0x200).sub_addr(PhysAddr::from(0x100)), 0x100);

    // wrapping_sub_addr
    ax_assert_eq!(VirtAddr::from(0x100).wrapping_sub_addr(VirtAddr::from(0x200)), usize::MAX - 0xff);

    // overflowing_sub_addr
    let (diff, ovf) = VirtAddr::from(0x300).overflowing_sub_addr(VirtAddr::from(0x100));
    ax_assert_eq!(diff, 0x200);
    ax_assert!(!ovf);
    let (diff, ovf) = VirtAddr::from(0x100).overflowing_sub_addr(VirtAddr::from(0x300));
    ax_assert_eq!(diff, usize::MAX - 0x1ff);
    ax_assert!(ovf);

    // checked_sub_addr
    ax_assert_eq!(VirtAddr::from(0x300).checked_sub_addr(VirtAddr::from(0x100)), Some(0x200));
    ax_assert_eq!(VirtAddr::from(0x100).checked_sub_addr(VirtAddr::from(0x300)), None);
}

#[axtest]
fn memory_addr_page_size_constants_hold() {
    ax_assert!(crate::memory_addr_page_size_constants_hold());
}
