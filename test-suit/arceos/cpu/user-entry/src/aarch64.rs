//! Real EL0 PMU reads, revocation and interrupted user register snapshots.

use core::sync::atomic::{AtomicUsize, Ordering};
use std::os::arceos::{modules::ax_hal, thread};

use ax_cpu::{
    VirtAddr,
    paging::MappingFlags,
    pmu::{CounterId, EventConfig, Pmu},
    user::{ReturnReason, UserContext},
};

const CODE: usize = 0x1000_0000;
const STACK: usize = CODE + 4096;
const STACK_TOP: usize = STACK + 4096;
static USER_IRQ_PC: AtomicUsize = AtomicUsize::new(0);

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
    ".global cpu_pmu_user_end",
    "cpu_pmu_user_end:",
);
unsafe extern "C" {
    fn cpu_pmu_user_start();
    fn cpu_pmu_user_loop();
    fn cpu_pmu_user_end();
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

fn user_thread(loop_offset: usize, cpu: usize) {
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
    context.set_ip(CODE + loop_offset);
    while USER_IRQ_PC.load(Ordering::Acquire) == 0 {
        assert!(matches!(context.enter().unwrap(), ReturnReason::Interrupt));
    }
    ax_hal::irq::disable_irq(handle).unwrap();
    ax_hal::irq::synchronize_irq(handle).unwrap();
    ax_hal::irq::free_irq(handle).unwrap();
}

fn run_cpu(cpu: usize) {
    USER_IRQ_PC.store(0, Ordering::Release);
    let mut code = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    let mut stack = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    code.zero();
    stack.zero();
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
    let owner = std::os::arceos::sync::IrqSafeMutex::new((table, code, stack));
    let address_space = thread::TaskAddressSpace::new(root, owner).unwrap();
    let loop_offset = cpu_pmu_user_loop as *const () as usize - start;
    // SAFETY: the runtime address-space token retains every table and backing
    // page until task and lazy-CPU leases retire; there is no external extension.
    let task = unsafe {
        thread::spawn_raw_with_extension_in_address_space(
            move || user_thread(loop_offset, cpu),
            "cpu-pmu-user".into(),
            0x10000,
            None,
            address_space,
        )
        .unwrap()
    };
    assert_eq!(thread::join_thread(task).unwrap(), 0);
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
        run_cpu(cpu);
    }
    std::println!("CPU_PMU_USER_OK");
}
