//! Distinct live address spaces sharing a numeric ASID must remain isolated.

use core::sync::atomic::{AtomicUsize, Ordering};
use std::{
    os::arceos::{
        api::task::{self as task_api, AxWaitQueueHandle},
        modules::ax_hal,
        task::{
            sched::{CpuId, CpuSet, RtPriority, SchedulePolicy},
            thread::ThreadState,
        },
        thread,
    },
    sync::Arc,
};

use ax_cpu::{VirtAddr, paging::MappingFlags, user::UserContext};

use super::{
    CODE, DATA, ORIGINAL_WORD, REPLACEMENT_WORD, STACK, STACK_TOP, cpu_pmu_user_end,
    cpu_user_load_word, load_user_word,
};

struct Alternation {
    turn: AtomicUsize,
    start: [AxWaitQueueHandle; 2],
}

pub(super) fn run() {
    // Equal-priority FIFO tasks switch directly to each other: an intervening
    // kernel task would clear TTBR0 and hide missing incoming-ASID invalidation.
    super::super::pin_to(0);
    let mut affinity = CpuSet::empty(ax_hal::cpu_num());
    assert!(affinity.insert(CpuId::new(0)));
    let alternation = Arc::new(Alternation {
        turn: AtomicUsize::new(2),
        start: [AxWaitQueueHandle::new(), AxWaitQueueHandle::new()],
    });
    let tasks = [ORIGINAL_WORD, REPLACEMENT_WORD].map(|word| {
        let index = usize::from(word == REPLACEMENT_WORD);
        let address_space = address_space(index, word);
        let shared = alternation.clone();
        // SAFETY: the managed token owns the complete private user mapping.
        // Its activation leases retain all frames until hardware retirement.
        unsafe {
            thread::prepare_user_thread(
                thread::builder("cpu-asid-reuse".into())
                    .stack_size(0x10000)
                    .policy(SchedulePolicy::fifo(RtPriority::new(80).unwrap()))
                    .affinity(affinity.clone()),
                move || alternate(index, word, shared),
                thread::UserContextOptions::new(address_space),
            )
            .unwrap()
        }
        .publish()
        .unwrap()
    });
    let started = std::time::Instant::now();
    while tasks
        .iter()
        .any(|task| task.state() != ThreadState::Blocked)
    {
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        std::thread::yield_now();
    }
    alternation.turn.store(0, Ordering::Release);
    assert_eq!(task_api::ax_wait_queue_wake(&alternation.start[0], 1), 1);
    for task in tasks {
        assert_eq!(task.join().unwrap(), 0);
    }
    std::println!("CPU_USER_ASID_REUSE_OK");
}

fn alternate(index: usize, word: usize, shared: Arc<Alternation>) {
    assert!(!task_api::ax_wait_queue_wait_until(
        &shared.start[index],
        || shared.turn.load(Ordering::Acquire) == index,
        None,
    ));
    let mut context = thread::UserExecutionContext::bind(UserContext::new(
        CODE,
        VirtAddr::from_usize(STACK_TOP),
        0,
    ))
    .unwrap();
    let started = std::time::Instant::now();
    for round in 0..32 {
        while shared.turn.load(Ordering::Acquire) != index {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::yield_now();
        }
        assert_eq!(
            load_user_word(&mut context, CODE),
            word,
            "ASID reuse exposed another live address space's data"
        );
        shared.turn.store(1 - index, Ordering::Release);
        if index == 0 && round == 0 {
            assert_eq!(task_api::ax_wait_queue_wake(&shared.start[1], 1), 1);
        }
        std::thread::yield_now();
    }
}

fn address_space(index: usize, word: usize) -> thread::TaskAddressSpace {
    let mut code = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    let mut stack = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    let mut data = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    code.zero();
    stack.zero();
    data.zero();
    let start = cpu_user_load_word as *const () as usize;
    let length = (cpu_pmu_user_end as *const () as usize)
        .checked_sub(start)
        .unwrap();
    assert!(length <= 4096);
    // SAFETY: code and data are disjoint unpublished, initialized allocations.
    // The linker labels bound static instructions; the word is naturally aligned.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, code.start_vaddr().as_mut_ptr(), length);
        data.start_vaddr()
            .as_mut_ptr()
            .cast::<usize>()
            .write_volatile(word);
        ax_cpu::cache::clean_dcache_range_to_pou(
            ax_cpu::cache::CacheRange::new(code.start_vaddr().as_usize().into(), length).unwrap(),
        );
    }
    ax_cpu::cache::flush_icache_all();
    let mut table = ax_hal::paging::PageTable::new(ax_hal::paging::PagingAllocator).unwrap();
    for (virtual_address, page, flags) in [
        (
            CODE,
            &code,
            MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER,
        ),
        (
            STACK,
            &stack,
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        ),
        (DATA, &data, MappingFlags::READ | MappingFlags::USER),
    ] {
        table
            .map(&ax_hal::paging::MapConfig {
                vaddr: virtual_address.into(),
                paddr: ax_hal::mem::virt_to_phys(page.start_vaddr().as_usize().into()),
                size: 4096,
                pte: flags,
                allow_huge: false,
                flush: false,
            })
            .unwrap();
    }
    let installed = ax_hal::context::InstalledAddressSpace::user(
        100 + index as u64,
        table.root_paddr(),
        1,
        index as u64,
        0,
        ax_hal::context::InstalledAddressSpaceMode::Tagged,
    )
    .unwrap();
    // SAFETY: all reachable tables and mapped pages move into the runtime owner.
    unsafe { super::super::managed::new(installed, (table, code, stack, data)).0 }
}
