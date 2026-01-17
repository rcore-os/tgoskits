use std::{
    alloc::{self, Layout},
    cell::RefCell,
    collections::{HashMap, HashSet},
    marker::PhantomData,
};

use memory_addr::{PhysAddr, VirtAddr};
use page_table_entry::{GenericPTE, MappingFlags};
use page_table_multiarch::{PageSize, PageTable64, PagingHandler, PagingMetaData, PagingResult};
use rand::{Rng, SeedableRng, rngs::SmallRng};

/// Creates a layout for allocating `num` pages with alignment of `2^align_pow2`
/// pages.
const fn pages_layout(num: usize, align: usize) -> Layout {
    if !align.is_power_of_two() {
        panic!("alignment must be a power of two");
    }
    if align % 4096 != 0 {
        panic!("alignment must be a multiple of 4K");
    }
    unsafe { Layout::from_size_align_unchecked(4096 * num, align) }
}

const PAGE_LAYOUT: Layout = pages_layout(1, 4096);

thread_local! {
    static ALLOCATED: RefCell<HashSet<usize>> = RefCell::default();
    static ALIGN: RefCell<HashMap<usize, usize>> = RefCell::default();
}

struct TrackPagingHandler<M: PagingMetaData>(PhantomData<M>);

impl<M: PagingMetaData> PagingHandler for TrackPagingHandler<M> {
    fn alloc_frame() -> Option<PhysAddr> {
        let ptr = unsafe { alloc::alloc(PAGE_LAYOUT) } as usize;
        assert!(
            ptr <= M::PA_MAX_ADDR,
            "allocated frame address exceeds PA_MAX_ADDR"
        );
        ALLOCATED.with_borrow_mut(|it| it.insert(ptr));
        Some(PhysAddr::from_usize(ptr))
    }

    fn alloc_frames(num: usize, align: usize) -> Option<PhysAddr> {
        let layout = pages_layout(num, align);
        let ptr = unsafe { alloc::alloc(layout) } as usize;
        assert!(
            ptr <= M::PA_MAX_ADDR,
            "allocated frame address exceeds PA_MAX_ADDR"
        );
        ALLOCATED.with_borrow_mut(|it| {
            for i in 0..num {
                it.insert(ptr + i * 4096);
            }
        });
        ALIGN.with_borrow_mut(|it| {
            it.insert(ptr, align);
        });
        Some(PhysAddr::from_usize(ptr))
    }

    fn dealloc_frame(paddr: PhysAddr) {
        let ptr = paddr.as_usize();
        ALLOCATED.with_borrow_mut(|it| {
            assert!(it.remove(&ptr), "dealloc a frame that was not allocated");
        });
        unsafe {
            alloc::dealloc(ptr as _, PAGE_LAYOUT);
        }
    }

    fn dealloc_frames(paddr: PhysAddr, num: usize) {
        let ptr = paddr.as_usize();
        ALLOCATED.with_borrow_mut(|it| {
            for i in 0..num {
                let addr = ptr + i * 4096;
                assert!(it.remove(&addr), "dealloc a frame that was not allocated");
            }
        });
        let align = ALIGN.with_borrow_mut(|it| {
            it.remove(&ptr)
                .expect("dealloc frames that were not allocated")
        });
        let layout = pages_layout(num, align);
        unsafe {
            alloc::dealloc(ptr as _, layout);
        }
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        assert!(paddr.as_usize() > 0);
        VirtAddr::from_usize(paddr.as_usize())
    }
}

fn run_test_for<M: PagingMetaData<VirtAddr = VirtAddr>, PTE: GenericPTE>() -> PagingResult<()> {
    ALLOCATED.with_borrow_mut(|it| {
        it.clear();
    });

    let vaddr_mask = ((1u64 << M::VA_MAX_BITS) - 1) & !0xfff;

    let mut table = PageTable64::<M, PTE, TrackPagingHandler<M>>::try_new().unwrap();
    let mut pages = HashSet::new();
    let mut rng = SmallRng::seed_from_u64(1234);

    for _ in 0..2048 {
        let mut cursor = table.cursor();
        if rng.random_ratio(3, 4) || pages.is_empty() {
            // insert a mapping
            let addr = loop {
                let addr = rng.random::<u64>() & vaddr_mask;
                if pages.insert(addr) {
                    break addr;
                }
            };
            cursor.map(
                VirtAddr::from_usize(addr as usize),
                PhysAddr::from_usize((rng.random::<u64>() & vaddr_mask) as usize),
                PageSize::Size4K,
                MappingFlags::READ | MappingFlags::WRITE,
            )?;
        } else {
            // remove a mapping
            let addr = *pages.iter().next().unwrap();
            cursor.unmap(VirtAddr::from_usize(addr as usize))?;
            pages.remove(&addr);
        }
    }

    drop(table);
    assert_eq!(
        ALLOCATED.with_borrow(|it| it.len()),
        0,
        "Some frames were not deallocated"
    );

    Ok(())
}

#[test]
#[cfg(any(target_arch = "x86_64", docsrs))]
fn test_dealloc_x86() -> PagingResult<()> {
    run_test_for::<
        page_table_multiarch::x86_64::X64PagingMetaData,
        page_table_entry::x86_64::X64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", docsrs))]
fn test_dealloc_riscv() -> PagingResult<()> {
    run_test_for::<
        page_table_multiarch::riscv::Sv39MetaData<VirtAddr>,
        page_table_entry::riscv::Rv64PTE,
    >()?;
    run_test_for::<
        page_table_multiarch::riscv::Sv48MetaData<VirtAddr>,
        page_table_entry::riscv::Rv64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "aarch64", docsrs))]
fn test_dealloc_aarch64() -> PagingResult<()> {
    run_test_for::<
        page_table_multiarch::aarch64::A64PagingMetaData,
        page_table_entry::aarch64::A64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "loongarch64", docsrs))]
fn test_dealloc_loongarch64() -> PagingResult<()> {
    run_test_for::<
        page_table_multiarch::loongarch64::LA64MetaData,
        page_table_entry::loongarch64::LA64PTE,
    >()?;
    Ok(())
}
