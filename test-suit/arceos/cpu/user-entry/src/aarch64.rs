//! Tagged EL0 restoration, PMU permissions and interrupted register snapshots.

mod reuse;

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::{
    os::arceos::{
        api::task::{self as task_api, AxWaitQueueHandle},
        modules::ax_hal,
        thread,
    },
    sync::Arc,
};

use ax_cpu::{
    VirtAddr,
    paging::MappingFlags,
    pmu::{CounterId, EventConfig, Pmu},
    user::{ReturnReason, UserContext},
};

const CODE: usize = 0x1000_0000;
const STACK: usize = CODE + 4096;
const STACK_TOP: usize = STACK + 4096;
const DATA: usize = STACK_TOP;
const ORIGINAL_WORD: usize = 0x1234_5678;
const REPLACEMENT_WORD: usize = 0x8765_4321;
static USER_IRQ_PC: AtomicUsize = AtomicUsize::new(0);
static PARK_READY: AtomicBool = AtomicBool::new(false);
static RESUME: AtomicBool = AtomicBool::new(false);
static PARK: AxWaitQueueHandle = AxWaitQueueHandle::new();

core::arch::global_asm!(
    ".section .text",
    ".balign 4",
    ".global cpu_pmu_user_start",
    "cpu_pmu_user_start:",
    "mov x29, sp",
    "mrs x0, pmevcntr0_el0",
    "svc #0",
    ".global cpu_pmu_user_loop",
    "cpu_pmu_user_loop:",
    "mov x29, sp",
    "2: b 2b",
    ".global cpu_user_load_word",
    "cpu_user_load_word:",
    "ldr x0, [x0]",
    "svc #0",
    ".global cpu_pmu_user_end",
    "cpu_pmu_user_end:",
);
unsafe extern "C" {
    fn cpu_pmu_user_start();
    fn cpu_pmu_user_loop();
    fn cpu_user_load_word();
    fn cpu_pmu_user_end();
}

struct UserProgram {
    interrupt_loop: usize,
    load_word: usize,
    hardware_tag: u16,
}

fn load_user_word(context: &mut thread::UserExecutionContext, entry: usize) -> usize {
    context.set_ip(entry);
    context.set_arg0(DATA);
    loop {
        match context.enter().unwrap() {
            ReturnReason::Interrupt => continue,
            ReturnReason::Syscall => return context.arg0(),
            other => panic!("EL0 data read failed: {other:?}"),
        }
    }
}

fn handle(_: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    // SAFETY: the affine test owns this PMU, and its user-entry register
    // session ended before IRQs could be taken. Native IRQ entry excludes reentry.
    let mut pmu = unsafe { Pmu::current() }.unwrap();
    if pmu.overflow_status() & (1 << 31) == 0 {
        return ax_hal::irq::IrqReturn::Unhandled;
    }
    pmu.disable(CounterId::CYCLE).unwrap();
    pmu.disable_overflow_irq(CounterId::CYCLE).unwrap();
    let snapshot = ax_hal::irq::interrupted_context().expect("user IRQ snapshot");
    assert_eq!(snapshot.privilege, ax_cpu::trap::InterruptedPrivilege::User);
    assert_eq!(snapshot.sp, STACK_TOP);
    assert_eq!(snapshot.fp, STACK_TOP);
    assert!((CODE..CODE + 32).contains(&snapshot.pc));
    USER_IRQ_PC.store(snapshot.pc, Ordering::Release);
    ax_hal::irq::IrqReturn::Handled
}

fn user_thread(program: UserProgram, cpu: usize, root: ax_cpu::PhysAddr) {
    super::pin_to(cpu);
    let mut context = thread::UserExecutionContext::bind(UserContext::new(
        CODE,
        VirtAddr::from_usize(STACK_TOP),
        0,
    ))
    .unwrap();
    ax_cpu::interrupt::disable_irqs();
    // SAFETY: this task exclusively owns all local counters and stays on its selected CPU.
    // All unused counters are stopped before pre-v3p9 EL0 permission is granted.
    unsafe {
        let mut pmu = Pmu::current().unwrap();
        pmu.reset();
        let counter = pmu.counter(0).unwrap();
        pmu.write(counter, 0x123456).unwrap();
        pmu.enable_user_access(1).unwrap();
    }
    ax_cpu::interrupt::enable_irqs();
    loop {
        match context.enter().unwrap() {
            ReturnReason::Interrupt => continue,
            ReturnReason::Syscall => break,
            other => panic!("authorized EL0 read failed: {other:?}"),
        }
    }
    assert_eq!(context.arg0(), 0x123456);
    ax_cpu::interrupt::disable_irqs();
    // SAFETY: same owner CPU; revoke authorization before the next user entry.
    unsafe { Pmu::current().unwrap().disable_user_access() };
    ax_cpu::interrupt::enable_irqs();
    assert_eq!(
        load_user_word(&mut context, program.load_word),
        ORIGINAL_WORD
    );
    PARK_READY.store(true, Ordering::Release);
    assert!(!task_api::ax_wait_queue_wait_until(
        &PARK,
        || RESUME.load(Ordering::Acquire),
        None,
    ));
    {
        let _irq = std::os::arceos::sync::IrqSaveGuard::new();
        let restored = ax_cpu::mmu::El1::read_user_address_space();
        assert_eq!(restored.root(), root);
        assert_eq!(restored.hardware_tag(), program.hardware_tag);
    }
    assert_eq!(
        load_user_word(&mut context, program.load_word),
        REPLACEMENT_WORD,
        "restored EL0 execution must observe the mapping replaced while parked"
    );
    context.set_ip(CODE);
    loop {
        match context.enter().unwrap() {
            ReturnReason::Interrupt => continue,
            ReturnReason::Exception(_) => break,
            other => panic!("revoked EL0 read did not trap: {other:?}"),
        }
    }
    assert_eq!(context.ip(), CODE + 4);
    let irq = ax_hal::pmu::irq().unwrap();
    let handle = ax_hal::irq::request_percpu_irq(
        irq,
        ax_hal::irq::CpuMask::from_cpu(ax_hal::irq::CpuId(cpu)),
        handle,
    )
    .unwrap();
    ax_cpu::interrupt::disable_irqs();
    // SAFETY: only EL0 execution increments this cycle counter. The session
    // ends before the prepared user entry unmasks IRQs; no PMU alias escapes.
    unsafe {
        let mut pmu = Pmu::current().unwrap();
        pmu.configure(
            CounterId::CYCLE,
            EventConfig {
                event: 0x11,
                exclude_user: false,
                exclude_kernel: true,
                include_hypervisor: false,
            },
        )
        .unwrap();
        pmu.preload(CounterId::CYCLE, 100_000).unwrap();
        pmu.enable_overflow_irq(CounterId::CYCLE).unwrap();
        pmu.enable(CounterId::CYCLE).unwrap();
        pmu.start();
    }
    ax_cpu::interrupt::enable_irqs();
    context.set_ip(program.interrupt_loop);
    while USER_IRQ_PC.load(Ordering::Acquire) == 0 {
        assert!(matches!(context.enter().unwrap(), ReturnReason::Interrupt));
    }
    ax_hal::irq::disable_irq(handle).unwrap();
    ax_hal::irq::synchronize_irq(handle).unwrap();
    ax_hal::irq::free_irq(handle).unwrap();
}

fn run_cpu(cpu: usize, hardware_tag: u16) {
    super::pin_to(cpu);
    USER_IRQ_PC.store(0, Ordering::Release);
    PARK_READY.store(false, Ordering::Release);
    RESUME.store(false, Ordering::Release);
    let mut code = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    let mut stack = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    let mut data = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    let mut replacement = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    code.zero();
    stack.zero();
    data.zero();
    replacement.zero();
    // SAFETY: these disjoint, aligned allocations are unpublished writable pages.
    unsafe {
        data.start_vaddr()
            .as_mut_ptr()
            .cast::<usize>()
            .write_volatile(ORIGINAL_WORD);
        replacement
            .start_vaddr()
            .as_mut_ptr()
            .cast::<usize>()
            .write_volatile(REPLACEMENT_WORD);
    }
    let start = cpu_pmu_user_start as *const () as usize;
    let end = cpu_pmu_user_end as *const () as usize;
    let length = end.checked_sub(start).unwrap();
    assert!(length <= 4096);
    // SAFETY: the labels delimit initialized static text and code owns a
    // disjoint writable page. No task can execute it until publication below.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, code.start_vaddr().as_mut_ptr(), length);
        ax_cpu::cache::clean_dcache_range_to_pou(
            ax_cpu::cache::CacheRange::new(code.start_vaddr().as_usize().into(), length).unwrap(),
        );
    }
    ax_cpu::cache::flush_icache_all();
    let mut table = ax_hal::paging::PageTable::new(ax_hal::paging::PagingAllocator).unwrap();
    for (virtual_address, physical, flags) in [
        (
            CODE,
            ax_hal::mem::virt_to_phys(code.start_vaddr().as_usize().into()),
            MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER,
        ),
        (
            STACK,
            ax_hal::mem::virt_to_phys(stack.start_vaddr().as_usize().into()),
            MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
        ),
        (
            DATA,
            ax_hal::mem::virt_to_phys(data.start_vaddr().as_usize().into()),
            MappingFlags::READ | MappingFlags::USER,
        ),
    ] {
        table
            .map(&ax_hal::paging::MapConfig {
                vaddr: virtual_address.into(),
                paddr: physical,
                size: 4096,
                pte: flags,
                allow_huge: false,
                flush: false,
            })
            .unwrap();
    }
    let root = table.root_paddr();
    let owner = Arc::new(std::os::arceos::sync::IrqSafeMutex::new((
        table,
        code,
        stack,
        data,
        replacement,
    )));
    let mode = if hardware_tag == 0 {
        ax_hal::context::InstalledAddressSpaceMode::FullFlush
    } else {
        ax_hal::context::InstalledAddressSpaceMode::Tagged
    };
    let installed = ax_hal::context::InstalledAddressSpace::user(
        cpu as u64 * 2 + u64::from(hardware_tag) + 1,
        root,
        hardware_tag,
        0,
        0,
        mode,
    )
    .unwrap();
    // SAFETY: owner retains the complete table and all old/new backing pages.
    // Their allocations remain stable until the runtime retires its last lease.
    let (address_space, cpu_state) = unsafe { super::managed::new(installed, owner.clone()) };
    let program = UserProgram {
        interrupt_loop: CODE + (cpu_pmu_user_loop as *const () as usize - start),
        load_word: CODE + (cpu_user_load_word as *const () as usize - start),
        hardware_tag,
    };
    // SAFETY: the runtime address-space token retains every table and backing
    // page until task and lazy-CPU leases retire; there is no external extension.
    let task = unsafe {
        thread::prepare_user_thread(
            thread::builder("cpu-pmu-user".into()).stack_size(0x10000),
            move || user_thread(program, cpu, root),
            thread::UserContextOptions::new(address_space),
        )
        .unwrap()
    }
    .publish()
    .unwrap();
    let started = std::time::Instant::now();
    while !PARK_READY.load(Ordering::Acquire)
        || task.state() != std::os::arceos::task::thread::ThreadState::Blocked
    {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "EL0 owner did not park"
        );
        std::thread::yield_now();
    }
    {
        let _irq = std::os::arceos::sync::IrqSaveGuard::new();
        let lazy = ax_cpu::mmu::El1::read_user_address_space();
        assert_eq!(lazy.root().as_usize(), 0);
        assert_eq!(lazy.hardware_tag(), 0);
    }
    assert_ne!(cpu_state.active_mask() & (1usize << cpu), 0);
    assert!(ax_hal::cpu_num() > 1);
    super::pin_to((cpu + 1) % ax_hal::cpu_num());
    {
        let mut backing = owner.lock();
        let replacement = ax_hal::mem::virt_to_phys(backing.4.start_vaddr().as_usize().into());
        // The sole user is parked on another CPU, whose active-mm lease must
        // remain a shootdown target even with its reserved lower root loaded.
        // Both old and new backing pages stay owned through task retirement.
        backing
            .0
            .remap_page(
                DATA.into(),
                replacement,
                MappingFlags::READ | MappingFlags::USER,
            )
            .unwrap();
    }
    ax_hal::cache::flush_tlb_range_on_cpus(cpu_state.active_mask(), DATA.into(), 4096).unwrap();
    RESUME.store(true, Ordering::Release);
    assert_eq!(task_api::ax_wait_queue_wake(&PARK, 1), 1);
    assert_eq!(task.join().unwrap(), 0);
    std::println!("CPU_USER_RESTORE_OK cpu={cpu} tag={hardware_tag}");
    std::println!("CPU_USER_REMOTE_REMAP_OK cpu={cpu} tag={hardware_tag}");
    std::println!(
        "CPU_PMU_USER_CORE cpu={cpu} pc={:#x}",
        USER_IRQ_PC.load(Ordering::Acquire)
    );
}

pub fn run() {
    for cpu in 0..ax_hal::cpu_num() {
        for _ in 0..300 {
            if ax_hal::irq::is_cpu_online(cpu) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ax_hal::irq::is_cpu_online(cpu));
        // Both installation modes must survive the same real lazy-mm transition.
        for hardware_tag in [0, 1] {
            run_cpu(cpu, hardware_tag);
        }
    }
    reuse::run();
    std::println!("CPU_PMU_USER_OK");
}
