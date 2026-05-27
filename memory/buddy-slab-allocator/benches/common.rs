#![allow(dead_code)]

use core::alloc::Layout;
use std::{
    alloc::{alloc, dealloc},
    sync::{
        Mutex, MutexGuard, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use buddy_slab_allocator::{
    __reset_global_allocator_singleton_for_tests, BuddyAllocator, GlobalAllocator, PerCpuSlab,
    SlabAllocResult, SlabAllocator, SlabDeallocResult, SlabPoolTrait,
    eii::{slab_pool_impl, virt_to_phys_impl},
};
use rand::{SeedableRng, rngs::StdRng};

pub const PAGE_SIZE: usize = 0x1000;
pub const HEAP_SIZE: usize = 64 * 1024 * 1024;
pub const OPERATIONS_PER_BATCH: usize = 256;
pub const FRAGMENTATION_PAGES: usize = 512;

const REGION_ALIGN: usize = 64 * 1024;
const BENCH_PAGE_SIZE: usize = 0x1000;
const MAX_BENCH_CPUS: usize = 64;

pub struct HostRegion {
    ptr: *mut u8,
    layout: Layout,
}

impl HostRegion {
    pub fn new(size: usize) -> Self {
        let layout = Layout::from_size_align(size, REGION_ALIGN).unwrap();
        let ptr = unsafe { alloc(layout) };
        assert!(!ptr.is_null(), "failed to allocate host region");
        Self { ptr, layout }
    }

    pub fn addr(&self) -> usize {
        self.ptr as usize
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for HostRegion {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

pub struct MockOs {
    cpu: AtomicUsize,
}

impl MockOs {
    pub const fn new() -> Self {
        Self {
            cpu: AtomicUsize::new(0),
        }
    }

    pub fn set_cpu(&self, cpu: usize) {
        self.cpu.store(cpu, Ordering::Relaxed);
    }
}

pub struct BenchContext {
    _guard: MutexGuard<'static, ()>,
}

struct BenchSlabPool {
    slabs: &'static [PerCpuSlab<BENCH_PAGE_SIZE>],
}

pub static MOCK_OS: MockOs = MockOs::new();

fn bench_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Drop for BenchContext {
    fn drop(&mut self) {
        __reset_global_allocator_singleton_for_tests();
    }
}

fn bench_cpu_slabs() -> &'static [PerCpuSlab<BENCH_PAGE_SIZE>] {
    static SLABS: OnceLock<Box<[PerCpuSlab<BENCH_PAGE_SIZE>]>> = OnceLock::new();
    SLABS.get_or_init(|| {
        (0..MAX_BENCH_CPUS)
            .map(|cpu| PerCpuSlab::new(cpu as u16))
            .collect::<Vec<_>>()
            .into_boxed_slice()
    })
}

fn reset_cpu_slabs(cpu_count: usize) {
    assert!(
        cpu_count <= MAX_BENCH_CPUS,
        "cpu_count exceeds bench slab pool"
    );
    for slab in &bench_cpu_slabs()[..cpu_count] {
        slab.reset();
    }
}

impl SlabPoolTrait for BenchSlabPool {
    fn current_slab(&self) -> &dyn buddy_slab_allocator::SlabTrait {
        &self.slabs[MOCK_OS.cpu.load(Ordering::Relaxed)]
    }

    fn owner_slab(&self, cpu_idx: usize) -> &dyn buddy_slab_allocator::SlabTrait {
        &self.slabs[cpu_idx]
    }
}

fn bench_slab_pool_ref() -> &'static BenchSlabPool {
    static POOL: OnceLock<BenchSlabPool> = OnceLock::new();
    POOL.get_or_init(|| BenchSlabPool {
        slabs: bench_cpu_slabs(),
    })
}

#[virt_to_phys_impl]
fn bench_virt_to_phys(vaddr: usize) -> usize {
    vaddr
}

#[slab_pool_impl]
fn bench_slab_pool() -> &'static dyn SlabPoolTrait {
    bench_slab_pool_ref()
}

pub fn seeded_rng() -> StdRng {
    StdRng::from_seed([0; 32])
}

pub struct BuddyHarness {
    _region: HostRegion,
    pub allocator: BuddyAllocator<PAGE_SIZE>,
}

impl BuddyHarness {
    pub fn new(heap_size: usize) -> Self {
        let region_size =
            heap_size + BuddyAllocator::<PAGE_SIZE>::required_meta_size(heap_size) + PAGE_SIZE * 4;
        let mut region = HostRegion::new(region_size);
        let mut allocator = BuddyAllocator::<PAGE_SIZE>::new();
        unsafe {
            allocator.init(region.as_mut_slice()).unwrap();
        }
        Self {
            _region: region,
            allocator,
        }
    }
}

pub struct SlabHarness {
    _region: HostRegion,
    buddy: BuddyAllocator<PAGE_SIZE>,
    slab: SlabAllocator<PAGE_SIZE>,
}

impl SlabHarness {
    pub fn new(heap_size: usize) -> Self {
        let region_size =
            heap_size + BuddyAllocator::<PAGE_SIZE>::required_meta_size(heap_size) + PAGE_SIZE * 4;
        let mut region = HostRegion::new(region_size);
        let mut buddy = BuddyAllocator::<PAGE_SIZE>::new();
        unsafe {
            buddy.init(region.as_mut_slice()).unwrap();
        }
        Self {
            _region: region,
            buddy,
            slab: SlabAllocator::new(),
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> core::ptr::NonNull<u8> {
        loop {
            match self.slab.alloc(layout).unwrap() {
                SlabAllocResult::Allocated(ptr) => return ptr,
                SlabAllocResult::NeedsSlab { size_class, pages } => {
                    let slab_bytes = pages * PAGE_SIZE;
                    let base = self.buddy.alloc_pages(pages, slab_bytes).unwrap();
                    self.slab.add_slab(size_class, base, slab_bytes, 0);
                }
            }
        }
    }

    pub fn dealloc(&mut self, ptr: core::ptr::NonNull<u8>, layout: Layout) {
        match self.slab.dealloc(ptr, layout) {
            SlabDeallocResult::Done => {}
            SlabDeallocResult::FreeSlab { base, pages } => {
                self.buddy.dealloc_pages(base, pages);
            }
        }
    }
}

pub struct GlobalHarness {
    _region: HostRegion,
    _ctx: BenchContext,
    pub allocator: GlobalAllocator<PAGE_SIZE>,
}

impl GlobalHarness {
    pub fn new(region_size: usize, cpu_count: usize) -> Self {
        assert_eq!(
            PAGE_SIZE, BENCH_PAGE_SIZE,
            "bench EII slab pool only supports PAGE_SIZE={BENCH_PAGE_SIZE:#x}"
        );
        let mut region = HostRegion::new(region_size);
        let allocator = GlobalAllocator::<PAGE_SIZE>::new();
        let guard = bench_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        MOCK_OS.set_cpu(0);
        reset_cpu_slabs(cpu_count);
        unsafe {
            allocator.init(region.as_mut_slice()).unwrap();
        }
        Self {
            _region: region,
            _ctx: BenchContext { _guard: guard },
            allocator,
        }
    }
}
