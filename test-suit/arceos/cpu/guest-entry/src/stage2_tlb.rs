//! A live guest must observe another CPU's completed stage-two remap.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_cpu::{
    PhysAddr, VirtAddr,
    paging::{MappingFlags, PageTableEntry, Stage2Pte},
    virtualization::{
        HostIrqConfig, PerCpu, Vcpu, enter_guest, guest_vector,
        invalidate_guest_translations_inner_shareable,
    },
};

static READY: AtomicUsize = AtomicUsize::new(0);
static STARTED: AtomicUsize = AtomicUsize::new(0);
static REPLACED: AtomicUsize = AtomicUsize::new(0);

core::arch::global_asm!(
    ".section .text",
    ".balign 4",
    ".global cpu_stage2_tlb_guest",
    "cpu_stage2_tlb_guest:",
    "ldr x3, [x0]",
    "mov x4, #1",
    "stlr x4, [x1]",
    "2: ldar x4, [x2]",
    "cbz x4, 2b",
    "ldr x4, [x0]",
    "hvc #0",
);
unsafe extern "C" {
    fn cpu_stage2_tlb_guest();
}

fn pin(cpu: usize) {
    use std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
    ax_set_current_affinity(AxCpuMask::one_shot(cpu)).unwrap();
    while ax_hal::percpu::this_cpu_id() != cpu {
        std::thread::yield_now();
    }
}

fn physical(address: usize) -> PhysAddr {
    ax_hal::mem::virt_to_phys(VirtAddr::from_usize(address))
}

fn page() -> ax_alloc::GlobalPage {
    let mut page = ax_alloc::GlobalPage::alloc_contiguous(1, 4096).unwrap();
    page.zero();
    page
}

pub fn run() {
    pin(0);
    while !ax_hal::irq::is_cpu_online(1) {
        std::thread::yield_now();
    }
    let root = page();
    let middle = page();
    let leaf = page();
    let old = page();
    let new = page();
    let access = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE;
    // SAFETY: all pages are exclusively owned, aligned and zeroed. The only
    // concurrent writer is started after publishing this complete tree.
    unsafe {
        old.start_vaddr().as_mut_ptr().cast::<u64>().write(0x123);
        new.start_vaddr().as_mut_ptr().cast::<u64>().write(0x456);
        root.start_vaddr()
            .as_mut_ptr()
            .cast::<Stage2Pte>()
            .add(1)
            .write(Stage2Pte::new_page(
                PhysAddr::from_usize(0x4000_0000),
                access,
                true,
            ));
        root.start_vaddr()
            .as_mut_ptr()
            .cast::<Stage2Pte>()
            .add(2)
            .write(Stage2Pte::new_table(physical(
                middle.start_vaddr().as_usize(),
            )));
        middle
            .start_vaddr()
            .as_mut_ptr()
            .cast::<Stage2Pte>()
            .write(Stage2Pte::new_table(physical(
                leaf.start_vaddr().as_usize(),
            )));
        leaf.start_vaddr()
            .as_mut_ptr()
            .cast::<Stage2Pte>()
            .write(Stage2Pte::new_page(
                physical(old.start_vaddr().as_usize()),
                access,
                false,
            ));
    }
    let leaf_address = leaf.start_vaddr().as_usize();
    let replacement = physical(new.start_vaddr().as_usize());
    let updater = std::thread::spawn(move || {
        pin(1);
        READY.store(1, Ordering::Release);
        while STARTED.load(Ordering::Acquire) == 0 {
            core::hint::spin_loop();
        }
        // SAFETY: this CPU is the sole descriptor writer. The guest has completed
        // its first load and waits on an independent identity-mapped flag until
        // the break-before-make sequence and both broadcast invalidations finish.
        // All old/new table and data pages remain owned by the joining thread.
        unsafe {
            let entry = leaf_address as *mut Stage2Pte;
            (entry as *mut u64).write_volatile(0);
            invalidate_guest_translations_inner_shareable();
            entry.write_volatile(Stage2Pte::new_page(replacement, access, false));
            invalidate_guest_translations_inner_shareable();
        }
        REPLACED.store(1, Ordering::Release);
    });
    // The spawned task initially inherits CPU 0 affinity. Let it execute its
    // affinity change before masking CPU 0 IRQs and entering the guest.
    while READY.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    let mut guest = Vcpu::default();
    guest.context.elr = physical(cpu_stage2_tlb_guest as *const () as usize).as_usize() as u64;
    guest.context.gpr[0] = 0x8000_0000;
    guest.context.gpr[1] = physical(&STARTED as *const _ as usize).as_usize() as u64;
    guest.context.gpr[2] = physical(&REPLACED as *const _ as usize).as_usize() as u64;
    guest.system.hcr_el2 = (1 << 31) | (1 << 19) | 1;
    guest.system.sctlr_el1 = 0x30c5_0830;
    guest.system.cpacr_el1 = 3 << 20;
    // 39-bit IPA, starting at level one, 4-KiB granule, inner-shareable WB walks,
    // 40-bit output. The QEMU machine supplies 1 GiB of RAM below this limit.
    let paging = ax_cpu::virtualization::Stage2Config::new(
        physical(root.start_vaddr().as_usize()),
        3,
        39,
        40,
    )
    .unwrap();
    assert_eq!(
        ax_cpu::virtualization::Stage2Config::new(paging.root() + 1, 3, 39, 40),
        Err(ax_cpu::virtualization::VirtualizationError::InvalidRoot)
    );
    assert_eq!(
        ax_cpu::virtualization::Stage2Config::new(paging.root(), 4, 39, 40),
        Err(ax_cpu::virtualization::VirtualizationError::UnsupportedPaging)
    );
    guest.system.vtcr_el2 = paging.control();
    guest.system.vttbr_el2 = paging.table_base(1);
    guest.set_host_irq_interface(HostIrqConfig::gicv3());
    let enabled = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let mut cpu = PerCpu::new();
    // SAFETY: CPU 0 remains pinned, IRQs are masked, every root and executable
    // page remains live. This trusted guest accesses only the stated test pages.
    let exit = unsafe {
        cpu.enable(guest_vector()).unwrap();
        let exit = enter_guest(&mut guest);
        cpu.disable().unwrap();
        exit
    };
    if enabled {
        ax_cpu::interrupt::enable_irqs();
    }
    updater.join().unwrap();
    assert_eq!(
        exit.syndrome >> 26,
        0x16,
        "stage-two guest must exit by HVC"
    );
    assert_eq!(
        guest.context.gpr[3], 0x123,
        "initial translation must be resident"
    );
    assert_eq!(
        guest.context.gpr[4], 0x456,
        "remote stage-two invalidation must replace the cached guest translation"
    );
    std::println!("CPU_STAGE2_TLB_OK");
}
