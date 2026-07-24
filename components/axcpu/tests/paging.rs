use std::{
    alloc::{self, Layout},
    cell::RefCell,
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_cpu::paging::{
    PageFrameProvider, PageSize, PageTable64, PagingMetaData, PagingResult, TlbInvalidator,
    TlbScope,
    entry::{GenericPTE, MappingFlags},
};
use ax_memory_addr::{PhysAddr, VirtAddr};
use rand::{RngExt, SeedableRng, rngs::SmallRng};

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

impl<M: PagingMetaData> Clone for TrackPagingHandler<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: PagingMetaData> Copy for TrackPagingHandler<M> {}

impl<M: PagingMetaData> Default for TrackPagingHandler<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[cfg(target_arch = "x86_64")]
struct HostTestTlb;

#[cfg(target_arch = "x86_64")]
impl TlbInvalidator<VirtAddr> for HostTestTlb {
    const SCOPE: TlbScope = TlbScope::Local;

    fn invalidate(_vaddr: Option<VirtAddr>) {}
}

#[cfg(target_arch = "x86_64")]
struct RemoteTestTlb;

#[cfg(target_arch = "x86_64")]
static REMOTE_INVALIDATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "x86_64")]
static REMOTE_INVALIDATION_BATCHES: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "x86_64")]
impl TlbInvalidator<VirtAddr> for RemoteTestTlb {
    const SCOPE: TlbScope = TlbScope::RemoteIpi;

    fn invalidate(_vaddr: Option<VirtAddr>) {
        REMOTE_INVALIDATIONS.fetch_add(1, Ordering::Relaxed);
    }

    fn invalidate_list(vaddrs: &[VirtAddr]) {
        REMOTE_INVALIDATIONS.fetch_add(vaddrs.len(), Ordering::Relaxed);
        REMOTE_INVALIDATION_BATCHES.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(target_arch = "x86_64")]
struct HostTestMeta;

#[cfg(target_arch = "x86_64")]
impl PagingMetaData for HostTestMeta {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 52;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = VirtAddr;
    type Tlb = HostTestTlb;
}

#[cfg(target_arch = "x86_64")]
#[test]
fn architecture_alias_accepts_runtime_tlb_invalidator() {
    use ax_cpu::paging::x86_64::{X64PageTable, X64PagingMetaData};

    type RuntimeMeta = X64PagingMetaData<RemoteTestTlb>;
    type RuntimeTable = X64PageTable<TrackPagingHandler<RuntimeMeta>, RemoteTestTlb>;

    let mut table = RuntimeTable::try_new().unwrap();
    for (vaddr, paddr) in [(0x1000usize, 0x3000usize), (0x2000, 0x4000)] {
        table
            .cursor()
            .map(
                vaddr.into(),
                paddr.into(),
                PageSize::Size4K,
                MappingFlags::READ,
            )
            .unwrap();
    }
    REMOTE_INVALIDATIONS.store(0, Ordering::Relaxed);
    REMOTE_INVALIDATION_BATCHES.store(0, Ordering::Relaxed);
    let mut cursor = table.cursor();
    cursor.unmap(0x1000usize.into()).unwrap();
    cursor.unmap(0x2000usize.into()).unwrap();
    cursor.flush();

    assert_eq!(REMOTE_INVALIDATIONS.load(Ordering::Relaxed), 2);
    assert_eq!(REMOTE_INVALIDATION_BATCHES.load(Ordering::Relaxed), 1);
}

impl<M: PagingMetaData + 'static> PageFrameProvider for TrackPagingHandler<M> {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let ptr = unsafe { alloc::alloc(PAGE_LAYOUT) } as usize;
        assert!(
            ptr <= M::PA_MAX_ADDR,
            "allocated frame address exceeds PA_MAX_ADDR"
        );
        ALLOCATED.with_borrow_mut(|it| it.insert(ptr));
        Some(PhysAddr::from_usize(ptr))
    }

    fn alloc_frames(&self, num: usize, align: usize) -> Option<PhysAddr> {
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

    fn dealloc_frame(&self, paddr: PhysAddr) {
        let ptr = paddr.as_usize();
        ALLOCATED.with_borrow_mut(|it| {
            assert!(it.remove(&ptr), "dealloc a frame that was not allocated");
        });
        unsafe {
            alloc::dealloc(ptr as _, PAGE_LAYOUT);
        }
    }

    fn dealloc_frames(&self, paddr: PhysAddr, num: usize) {
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

    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
        assert!(paddr.as_usize() > 0);
        VirtAddr::from_usize(paddr.as_usize())
    }
}

fn run_test_for<M: PagingMetaData<VirtAddr = VirtAddr> + 'static, PTE: GenericPTE>()
-> PagingResult<()> {
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

#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", docsrs))]
fn run_aligned_root_test_for<M: PagingMetaData<VirtAddr = VirtAddr>, PTE: GenericPTE>()
-> PagingResult<()> {
    ALLOCATED.with_borrow_mut(|it| {
        it.clear();
    });
    ALIGN.with_borrow_mut(|it| {
        it.clear();
    });

    let table =
        PageTable64::<M, PTE, TrackPagingHandler<M>>::try_new_with_root(4, 4096 * 4).unwrap();
    assert_eq!(table.root_paddr().as_usize() % (4096 * 4), 0);
    assert_eq!(
        ALIGN.with_borrow(|it| it.get(&table.root_paddr().as_usize()).copied()),
        Some(4096 * 4)
    );

    drop(table);
    assert_eq!(
        ALLOCATED.with_borrow(|it| it.len()),
        0,
        "Some frames were not deallocated"
    );

    Ok(())
}

#[cfg(target_pointer_width = "32")]
fn run_test_for_32bit<M: PagingMetaData<VirtAddr = VirtAddr> + 'static, PTE: GenericPTE>()
-> PagingResult<()> {
    use ax_cpu::paging::PageTable32;
    ALLOCATED.with_borrow_mut(|it| {
        it.clear();
    });

    let vaddr_mask = ((1u64 << M::VA_MAX_BITS) - 1) & !0xfff;

    let mut table = PageTable32::<M, PTE, TrackPagingHandler<M>>::try_new().unwrap();
    let mut pages = HashSet::new();
    let mut rng = SmallRng::seed_from_u64(5678);
    for _ in 0..512 {
        // Fewer iterations for 32-bit to avoid address space exhaustion
        if rng.random_ratio(3, 4) || pages.is_empty() {
            // insert a mapping
            let addr = loop {
                let addr = rng.random::<u32>() & (vaddr_mask as u32);
                if pages.insert(addr as u64) {
                    break addr as u64;
                }
            };
            table
                .map(
                    VirtAddr::from_usize(addr as usize),
                    PhysAddr::from_usize((rng.random::<u32>() & (vaddr_mask as u32)) as usize),
                    PageSize::Size4K,
                    MappingFlags::READ | MappingFlags::WRITE,
                )?
                .ignore();
        } else {
            // remove a mapping
            let addr = *pages.iter().next().unwrap();
            table.unmap(VirtAddr::from_usize(addr as usize))?.2.ignore();
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
#[cfg(any(target_arch = "arm", docsrs))]
#[cfg(target_pointer_width = "32")]
fn test_dealloc_arm32() -> PagingResult<()> {
    run_test_for_32bit::<ax_cpu::paging::arm::A32PagingMetaData, ax_cpu::paging::entry::arm::A32PTE>(
    )?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "x86_64", docsrs))]
fn test_dealloc_x86() -> PagingResult<()> {
    run_test_for::<HostTestMeta, ax_cpu::paging::entry::x86_64::X64PTE>()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", docsrs))]
fn test_dealloc_riscv() -> PagingResult<()> {
    run_test_for::<
        ax_cpu::paging::riscv::Sv39MetaData<VirtAddr>,
        ax_cpu::paging::entry::riscv::Rv64PTE,
    >()?;
    run_test_for::<
        ax_cpu::paging::riscv::Sv48MetaData<VirtAddr>,
        ax_cpu::paging::entry::riscv::Rv64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "riscv32", target_arch = "riscv64", docsrs))]
fn test_aligned_root_riscv() -> PagingResult<()> {
    run_aligned_root_test_for::<
        ax_cpu::paging::riscv::Sv39MetaData<VirtAddr>,
        ax_cpu::paging::entry::riscv::Rv64PTE,
    >()?;
    run_aligned_root_test_for::<
        ax_cpu::paging::riscv::Sv48MetaData<VirtAddr>,
        ax_cpu::paging::entry::riscv::Rv64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "aarch64", docsrs))]
fn test_dealloc_aarch64() -> PagingResult<()> {
    run_test_for::<
        ax_cpu::paging::aarch64::A64PagingMetaData,
        ax_cpu::paging::entry::aarch64::A64PTE,
    >()?;
    Ok(())
}

#[test]
#[cfg(any(target_arch = "loongarch64", docsrs))]
fn test_dealloc_loongarch64() -> PagingResult<()> {
    run_test_for::<
        ax_cpu::paging::loongarch64::LA64MetaData,
        ax_cpu::paging::entry::loongarch64::LA64PTE,
    >()?;
    Ok(())
}
