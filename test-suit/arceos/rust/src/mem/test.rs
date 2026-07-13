use core::{
    alloc::Layout,
    ptr::{self, NonNull},
    slice,
};
use std::{
    alloc::{alloc, dealloc},
    collections::BTreeMap,
    format,
    os::arceos::task::{AxCpuMask, ax_set_current_affinity},
    println,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

use ax_hal::percpu::this_cpu_id;
use rand::{RngCore, SeedableRng, rngs::SmallRng};

const SLAB_LAYOUT_CASES: [LayoutCase; 9] = [
    LayoutCase::new(1, 1),
    LayoutCase::new(1, 8),
    LayoutCase::new(24, 8),
    LayoutCase::new(63, 64),
    LayoutCase::new(96, 32),
    LayoutCase::new(255, 128),
    LayoutCase::new(511, 256),
    LayoutCase::new(1024, 512),
    LayoutCase::new(2048, 2048),
];

const ALIGN_LAYOUT_CASES: [LayoutCase; 13] = [
    LayoutCase::new(1, 1),
    LayoutCase::new(1, 8),
    LayoutCase::new(3, 2),
    LayoutCase::new(7, 4),
    LayoutCase::new(15, 16),
    LayoutCase::new(24, 8),
    LayoutCase::new(63, 64),
    LayoutCase::new(255, 128),
    LayoutCase::new(511, 256),
    LayoutCase::new(1024, 512),
    LayoutCase::new(2048, 2048),
    LayoutCase::new(2049, 2048),
    LayoutCase::new(4097, 4096),
];

const VEC_LEN: usize = 200_000;
const BTREE_MAP_LEN: usize = 10_000;
const ALIGN_TEST_ROUNDS: usize = 3;
const PARALLEL_ALLOC_ROUNDS: usize = 4;
const REMOTE_FREE_ROUNDS: usize = 8;

#[derive(Clone, Copy, Debug)]
struct LayoutCase {
    size: usize,
    align: usize,
}

impl LayoutCase {
    const fn new(size: usize, align: usize) -> Self {
        Self { size, align }
    }

    fn layout(self) -> Layout {
        Layout::from_size_align(self.size, self.align).unwrap()
    }
}

#[derive(Clone, Copy, Debug)]
struct Allocation {
    ptr: usize,
    size: usize,
    align: usize,
    pattern: u8,
}

impl Allocation {
    fn layout(&self) -> Layout {
        Layout::from_size_align(self.size, self.align).unwrap()
    }

    fn as_non_null(&self) -> NonNull<u8> {
        NonNull::new(self.ptr as *mut u8).unwrap()
    }
}

unsafe fn alloc_raw(layout: Layout) -> NonNull<u8> {
    NonNull::new(unsafe { alloc(layout) })
        .unwrap_or_else(|| panic!("allocation failed for {layout:?}"))
}

unsafe fn dealloc_raw(ptr: NonNull<u8>, layout: Layout) {
    unsafe { dealloc(ptr.as_ptr(), layout) };
}

fn allocation_pattern(index: usize, round: usize) -> u8 {
    (((index * 37) + (round * 17)) as u8).wrapping_add(1)
}

fn alloc_and_fill(case: LayoutCase, pattern: u8) -> Allocation {
    let layout = case.layout();
    let ptr = unsafe { alloc_raw(layout) };
    assert_eq!(
        ptr.as_ptr() as usize & (case.align - 1),
        0,
        "allocation is not aligned to {} bytes",
        case.align
    );
    unsafe { ptr::write_bytes(ptr.as_ptr(), pattern, case.size) };
    Allocation {
        ptr: ptr.as_ptr() as usize,
        size: case.size,
        align: case.align,
        pattern,
    }
}

fn verify_block(block: &Allocation) {
    let bytes = unsafe { slice::from_raw_parts(block.ptr as *const u8, block.size) };
    for &byte in bytes {
        assert_eq!(byte, block.pattern, "allocation payload corrupted");
    }
}

unsafe fn free_block(block: Allocation) {
    unsafe { dealloc_raw(block.as_non_null(), block.layout()) };
}

fn test_vec(rng: &mut impl RngCore) {
    let mut values = Vec::with_capacity(VEC_LEN);
    for _ in 0..VEC_LEN {
        values.push(rng.next_u32());
    }
    values.sort();
    for window in values.windows(2) {
        assert!(window[0] <= window[1]);
    }
    println!("memtest: vec OK");
}

fn test_btree_map(rng: &mut impl RngCore) {
    let mut map = BTreeMap::new();
    for _ in 0..BTREE_MAP_LEN {
        let value = rng.next_u32();
        let key = format!("key_{value}");
        map.insert(key, value);
    }
    for (key, value) in &map {
        if let Some(key) = key.strip_prefix("key_") {
            assert_eq!(key.parse::<u32>().unwrap(), *value);
        }
    }
    println!("memtest: btree_map OK");
}

fn test_aligned_allocations() {
    for round in 0..ALIGN_TEST_ROUNDS {
        let mut allocations = Vec::with_capacity(ALIGN_LAYOUT_CASES.len());
        for (index, case) in ALIGN_LAYOUT_CASES.iter().enumerate() {
            let block = alloc_and_fill(*case, allocation_pattern(index, round));
            verify_block(&block);
            allocations.push(block);
        }

        while let Some(block) = allocations.pop() {
            verify_block(&block);
            unsafe { free_block(block) };
        }
    }
    println!("memtest: aligned allocations OK");
}

fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin current task to CPU {cpu_id}"
    );
    for _ in 0..256 {
        if this_cpu_id() == cpu_id {
            return;
        }
        thread::yield_now();
    }
    assert_eq!(
        this_cpu_id(),
        cpu_id,
        "task did not migrate to CPU {cpu_id}"
    );
}

fn test_parallel_allocations() {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 2 {
        println!("memtest: parallel allocations skipped on single CPU");
        return;
    }

    let worker_count = cpu_num;
    let ready = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(worker_count);

    for worker_id in 0..worker_count {
        let ready = ready.clone();
        tasks.push(thread::spawn(move || {
            let cpu_id = worker_id % cpu_num;
            pin_current_to_cpu(cpu_id);

            ready.fetch_add(1, Ordering::AcqRel);
            while ready.load(Ordering::Acquire) < worker_count {
                thread::yield_now();
            }

            for round in 0..PARALLEL_ALLOC_ROUNDS {
                let mut blocks = Vec::with_capacity(SLAB_LAYOUT_CASES.len());
                for (index, case) in SLAB_LAYOUT_CASES.iter().enumerate() {
                    let pattern = allocation_pattern(worker_id + index, round);
                    blocks.push(alloc_and_fill(*case, pattern));
                    if index % 3 == 0 {
                        thread::yield_now();
                    }
                }

                while let Some(block) = blocks.pop() {
                    verify_block(&block);
                    unsafe { free_block(block) };
                }
            }
        }));
    }

    for task in tasks {
        task.join().unwrap();
    }
    println!("memtest: parallel allocations OK");
}

fn test_cross_cpu_free() {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num < 2 {
        println!("memtest: cross CPU free skipped on single CPU");
        return;
    }

    let owner_cpu = 0;
    let remote_cpu = cpu_num - 1;
    pin_current_to_cpu(owner_cpu);

    let mut remote_blocks = Vec::with_capacity(SLAB_LAYOUT_CASES.len() * REMOTE_FREE_ROUNDS);
    for round in 0..REMOTE_FREE_ROUNDS {
        for (index, case) in SLAB_LAYOUT_CASES.iter().enumerate() {
            let pattern = allocation_pattern(index + SLAB_LAYOUT_CASES.len(), round);
            remote_blocks.push(alloc_and_fill(*case, pattern));
        }
        thread::yield_now();
    }

    thread::spawn(move || {
        pin_current_to_cpu(remote_cpu);
        for (index, block) in remote_blocks.into_iter().enumerate() {
            verify_block(&block);
            unsafe { free_block(block) };
            if index % 8 == 0 {
                thread::yield_now();
            }
        }
    })
    .join()
    .unwrap();

    pin_current_to_cpu(owner_cpu);
    let mut recycled = Vec::with_capacity(SLAB_LAYOUT_CASES.len() * REMOTE_FREE_ROUNDS);
    for round in 0..REMOTE_FREE_ROUNDS {
        for (index, case) in SLAB_LAYOUT_CASES.iter().enumerate() {
            let pattern = allocation_pattern(index + 2 * SLAB_LAYOUT_CASES.len(), round);
            recycled.push(alloc_and_fill(*case, pattern));
        }
        thread::yield_now();
    }

    while let Some(block) = recycled.pop() {
        verify_block(&block);
        unsafe { free_block(block) };
    }
    println!("memtest: cross CPU free OK");
}

pub fn run() -> crate::TestResult {
    let mut rng = SmallRng::seed_from_u64(0xdead_beef);
    test_vec(&mut rng);
    test_btree_map(&mut rng);
    test_aligned_allocations();
    test_parallel_allocations();
    test_cross_cpu_free();
    Ok(())
}
