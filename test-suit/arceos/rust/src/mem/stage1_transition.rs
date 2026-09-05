use core::{alloc::Layout, ptr::NonNull};
use std::{
    os::arceos::api::{
        mem::{ax_alloc, ax_dealloc},
        modules::{
            ax_hal::{
                mem::{kernel_aspace, virt_to_phys},
                paging::MappingFlags,
                percpu::this_cpu_id,
                trap::{PageFaultFlags, set_page_fault_handler},
            },
            ax_runtime::kernel_mapping::{
                map_kernel_pages, map_kernel_range, protect_kernel_range, query_kernel_mapping,
                unmap_kernel_range,
            },
        },
        task::{AxCpuMask, ax_set_current_affinity},
    },
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use ax_memory_addr::{MemoryAddr, VirtAddr};

const PAGE_SIZE: usize = 4096;
const OLD_VALUE: u8 = 0x51;
const REMOTE_WRITE_VALUE: u8 = 0x72;
const STALE_FRAME_VALUE: u8 = 0xb4;
const REPLACEMENT_VALUE: u8 = 0x9d;
const PHASE_WAIT_YIELDS: usize = 100_000;
const REMOTE_WRITE_FAULT_IDLE: usize = 0;
const REMOTE_WRITE_FAULT_ARMED: usize = 1;
const REMOTE_WRITE_FAULT_HANDLING: usize = 2;
const REMOTE_WRITE_FAULT_HANDLED: usize = 3;
const REMOTE_WRITE_FAULT_FAILED: usize = 4;

static EXPECTED_WRITE_FAULT_ADDR: AtomicUsize = AtomicUsize::new(0);
static REMOTE_WRITE_FAULT_STATE: AtomicUsize = AtomicUsize::new(REMOTE_WRITE_FAULT_IDLE);
static PREVIOUS_PAGE_FAULT_HANDLER: AtomicUsize = AtomicUsize::new(0);

type PageFaultHandler = fn(VirtAddr, PageFaultFlags) -> bool;

fn delegate_page_fault(vaddr: VirtAddr, flags: PageFaultFlags) -> bool {
    let previous = PREVIOUS_PAGE_FAULT_HANDLER.load(Ordering::Acquire);
    if previous == 0 {
        return false;
    }
    // SAFETY: this atomic only receives the `PageFaultHandler` returned by
    // `set_page_fault_handler`, and the test restores it before clearing the
    // slot.
    let handler = unsafe { core::mem::transmute::<usize, PageFaultHandler>(previous) };
    handler(vaddr, flags)
}

fn handle_remote_write_fault(vaddr: VirtAddr, flags: PageFaultFlags) -> bool {
    if vaddr.as_usize() != EXPECTED_WRITE_FAULT_ADDR.load(Ordering::Acquire)
        || !flags.contains(PageFaultFlags::WRITE)
        || flags.contains(PageFaultFlags::USER)
    {
        return delegate_page_fault(vaddr, flags);
    }
    if REMOTE_WRITE_FAULT_STATE
        .compare_exchange(
            REMOTE_WRITE_FAULT_ARMED,
            REMOTE_WRITE_FAULT_HANDLING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return delegate_page_fault(vaddr, flags);
    }

    let writable = MappingFlags::READ | MappingFlags::WRITE;
    if protect_kernel_range(vaddr.align_down_4k(), PAGE_SIZE, writable).is_err() {
        REMOTE_WRITE_FAULT_STATE.store(REMOTE_WRITE_FAULT_FAILED, Ordering::Release);
        return false;
    }
    REMOTE_WRITE_FAULT_STATE.store(REMOTE_WRITE_FAULT_HANDLED, Ordering::Release);
    true
}

struct OwnedTestFrame {
    pointer: NonNull<u8>,
}

impl OwnedTestFrame {
    fn allocate() -> Self {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: this owner releases the allocation with the same layout only
        // after every temporary linear alias has been unmapped.
        let pointer = unsafe { ax_alloc(layout) }.expect("failed to allocate a test frame");
        // SAFETY: the allocation covers one writable page.
        unsafe { pointer.as_ptr().write_bytes(0, PAGE_SIZE) };
        Self { pointer }
    }

    fn paddr(&self) -> ax_memory_addr::PhysAddr {
        virt_to_phys((self.pointer.as_ptr() as usize).into())
    }

    fn write(&self, value: u8) {
        // SAFETY: this owner retains a writable page for its complete lifetime.
        unsafe { self.pointer.as_ptr().write_volatile(value) };
    }
}

impl Drop for OwnedTestFrame {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: the pointer was allocated with this exact layout and every
        // test alias is removed before the owner reaches Drop.
        unsafe { ax_dealloc(self.pointer, layout) };
    }
}

fn pin_current_to_cpu(cpu_id: usize) {
    assert!(
        ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
        "failed to pin stage-1 test task to CPU {cpu_id}"
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
        "stage-1 test task did not migrate to CPU {cpu_id}"
    );
}

fn wait_for_phase(phase: &AtomicUsize, expected: usize) {
    for _ in 0..PHASE_WAIT_YIELDS {
        if phase.load(Ordering::Acquire) >= expected {
            return;
        }
        thread::yield_now();
    }
    panic!("remote stage-1 observer did not reach phase {expected}");
}

pub fn run() -> crate::TestResult {
    let cpu_count = thread::available_parallelism().unwrap().get();
    if cpu_count < 2 {
        return Err("SMP kernel stage-1 transition test requires two CPUs");
    }
    let controller_cpu = 0;
    let remote_cpu = cpu_count - 1;
    pin_current_to_cpu(controller_cpu);

    let (kernel_base, kernel_size) = kernel_aspace();
    assert!(
        kernel_size >= 4 * PAGE_SIZE,
        "kernel stage-1 window is too small for the transition test"
    );
    let flags = MappingFlags::READ | MappingFlags::WRITE;
    let original_frame = OwnedTestFrame::allocate();
    let replacement_frame = OwnedTestFrame::allocate();
    assert_ne!(original_frame.paddr(), replacement_frame.paddr());
    let mapping = map_kernel_pages(kernel_base, &[original_frame.paddr()], flags)
        .expect("failed to map the original test frame");
    let mapping_ptr = mapping.as_mut_ptr();
    // SAFETY: `mapping` owns a populated writable page until the controller
    // unmaps it after the remote task has stopped accessing this generation.
    unsafe { mapping_ptr.write_volatile(OLD_VALUE) };

    let phase = Arc::new(AtomicUsize::new(0));
    let remote_phase = Arc::clone(&phase);
    let mapping_addr = mapping.as_usize();
    let remote = thread::spawn(move || {
        pin_current_to_cpu(remote_cpu);
        let pointer = mapping_addr as *mut u8;
        // SAFETY: phase 0 keeps the original mapping published and writable.
        assert_eq!(unsafe { pointer.read_volatile() }, OLD_VALUE);
        remote_phase.store(1, Ordering::Release);

        wait_for_phase(&remote_phase, 2);
        // SAFETY: phase 2 deliberately leaves the PTE read-only. A correct
        // remote shootdown therefore enters the temporary fault handler,
        // which restores WRITE before returning and retrying this store.
        unsafe { pointer.write_volatile(REMOTE_WRITE_VALUE) };
        remote_phase.store(3, Ordering::Release);

        wait_for_phase(&remote_phase, 4);
        // SAFETY: phase 4 publishes a replacement mapping at the same VA only
        // after synchronous unmap confirmation and initialization.
        assert_eq!(unsafe { pointer.read_volatile() }, REPLACEMENT_VALUE);
        remote_phase.store(5, Ordering::Release);
    });

    wait_for_phase(&phase, 1);
    protect_kernel_range(mapping, PAGE_SIZE, MappingFlags::READ)
        .expect("failed to remove write permission from the kernel mapping");
    let (protected_flags, protected_page_size) =
        query_kernel_mapping(mapping).expect("failed to query the protected kernel mapping");
    assert_eq!(protected_page_size, PAGE_SIZE);
    assert!(!protected_flags.contains(MappingFlags::WRITE));

    let previous_page_fault_handler = set_page_fault_handler(handle_remote_write_fault);
    PREVIOUS_PAGE_FAULT_HANDLER.store(previous_page_fault_handler as usize, Ordering::Release);
    EXPECTED_WRITE_FAULT_ADDR.store(mapping_addr, Ordering::Release);
    REMOTE_WRITE_FAULT_STATE.store(REMOTE_WRITE_FAULT_ARMED, Ordering::Release);
    phase.store(2, Ordering::Release);

    wait_for_phase(&phase, 3);
    assert_eq!(
        REMOTE_WRITE_FAULT_STATE.load(Ordering::Acquire),
        REMOTE_WRITE_FAULT_HANDLED,
        "the remote store bypassed the read-only PTE through a stale writable TLB entry"
    );
    let replaced_handler = set_page_fault_handler(previous_page_fault_handler);
    assert_eq!(
        replaced_handler as *const () as usize,
        handle_remote_write_fault as *const () as usize
    );
    EXPECTED_WRITE_FAULT_ADDR.store(0, Ordering::Release);
    PREVIOUS_PAGE_FAULT_HANDLER.store(0, Ordering::Release);
    REMOTE_WRITE_FAULT_STATE.store(REMOTE_WRITE_FAULT_IDLE, Ordering::Release);
    // SAFETY: phase 3 follows the remote write and the mapping remains valid.
    assert_eq!(unsafe { mapping_ptr.read_volatile() }, REMOTE_WRITE_VALUE);
    unmap_kernel_range(mapping, PAGE_SIZE)
        .expect("failed to synchronously unmap the original kernel mapping");

    // Retain and overwrite the old physical frame through its allocator-owned
    // direct mapping, then reuse the exact VA with a different owned frame.
    // This makes a stale remote translation observable without relying on any
    // allocator reuse policy.
    original_frame.write(STALE_FRAME_VALUE);
    map_kernel_range(mapping, replacement_frame.paddr(), PAGE_SIZE, flags)
        .expect("failed to remap the released kernel VA");
    // SAFETY: the original VA now aliases `replacement_frame`, which remains
    // owned by this test until after the mapping is removed.
    unsafe { mapping_ptr.write_volatile(REPLACEMENT_VALUE) };
    phase.store(4, Ordering::Release);

    wait_for_phase(&phase, 5);
    remote
        .join()
        .expect("remote stage-1 mapping observer must exit cleanly");
    unmap_kernel_range(mapping, PAGE_SIZE)
        .expect("failed to release the replacement kernel mapping");
    pin_current_to_cpu(controller_cpu);
    Ok(())
}
