use std::boxed::Box;

use ax_cpu::{
    VirtAddr, interrupt,
    registers::{self, FpuState},
    virtualization::{EntryAddresses, ExitKind, PerCpu, Vcpu, entry_addresses},
};
use ax_hal::paging::{MapConfig, MappingFlags, PageTable, PagingAllocator};

core::arch::global_asm!(
    ".section .text.cpu_test_guest, \"ax\"",
    ".balign 4096",
    ".global cpu_test_lvz_guest",
    "cpu_test_lvz_guest:",
    "movfr2gr.d $a1, $f0",
    "li.d $a0, 0x123",
    "movgr2fr.d $f0, $a0",
    "hvcl 0",
    "movfr2gr.d $a1, $f0",
    "li.d $a0, 0x456",
    "movgr2fr.d $f0, $a0",
    "hvcl 0",
    "ld.d $a0, $a2, 0",
    "hvcl 0",
    ".global cpu_test_lvz_busy_guest",
    "cpu_test_lvz_busy_guest:",
    "rdtime.d $a0, $zero",
    "add.d $a1, $a0, $a1",
    "2:",
    "rdtime.d $a0, $zero",
    "bltu $a0, $a1, 2b",
    "hvcl 0",
);

unsafe extern "C" {
    fn cpu_test_lvz_guest();
    fn cpu_test_lvz_busy_guest();
}

fn direct_alias(address: VirtAddr) -> VirtAddr {
    VirtAddr::from_usize(0x9000_0000_0000_0000 | ax_hal::mem::virt_to_phys(address).as_usize())
}

#[repr(C, align(4096))]
struct DataPage([u64; 512]);

fn timer_irq_enabled() -> bool {
    let ecfg: usize;
    // SAFETY: ECFG is a CPU-local control/status register. This read has no
    // memory or ownership preconditions and does not modify interrupt state.
    unsafe {
        core::arch::asm!("csrrd {}, 0x4", out(reg) ecfg, options(nomem, nostack));
    }
    ecfg & (1 << 11) != 0
}

pub fn run() {
    assert!(ax_cpu::capability::has_hypervisor_extension());
    let guest_address = VirtAddr::from_usize(0x10000);
    let code = ax_hal::mem::virt_to_phys(VirtAddr::from_usize(
        cpu_test_lvz_guest as *const () as usize,
    ));
    let mut table = PageTable::new(PagingAllocator).unwrap();
    table
        .map(&MapConfig {
            vaddr: guest_address,
            paddr: code,
            size: 4096,
            pte: MappingFlags::READ | MappingFlags::EXECUTE,
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    let first = Box::new(DataPage([0x1234; 512]));
    let second = Box::new(DataPage([0x5678; 512]));
    let data_address = VirtAddr::from_usize(0x20000);
    let mut state = Box::new(Vcpu::default());
    state.set_root(table.root_paddr()).unwrap();
    state.context.sepc = guest_address.as_usize();
    state.context.gcsr_era = guest_address.as_usize();
    state.context.gcsr_crmd = 1 << 3;
    state.context.gcsr_euen = 1;
    state.context.x[21] = 0xfeed;
    let entries = entry_addresses().map(direct_alias);
    let state_address = direct_alias(VirtAddr::from_usize((&raw mut *state) as usize));
    let irq = interrupt::irqs_enabled();
    let timer_irq = timer_irq_enabled();
    interrupt::disable_irqs();
    let anchor = registers::read_cpu_anchor();
    let tp = registers::read_tp();
    let mut original_fp = FpuState::default();
    original_fp.save();
    let mut host_fp = original_fp;
    host_fp.fp[0] = 0xabc;
    host_fp.restore();
    let mut cpu = PerCpu::new();
    // SAFETY: one CPU, no concurrent guests, IRQs masked. The boxed context,
    // page tables and trusted straight-line guest text outlive the binding;
    // only its code page is mapped into the nested translation.
    unsafe {
        cpu.enable().unwrap();
        // Pin the clockevent line enabled before binding so the host ECFG
        // snapshot taken by bind() carries LIE[11]. Masking it afterwards then
        // proves whether a full guest roundtrip respects a host mask applied
        // after the binding was established.
        interrupt::set_timer_irq_enabled(true);
        state.bind(EntryAddresses::new(entries).unwrap()).unwrap();
        // Root-mode interrupts are enabled while the guest runs. Mask the
        // host timer line until the test reaches the explicit IRQ phase so
        // the synchronous entry checks cannot consume a valid clock event.
        interrupt::set_timer_irq_enabled(false);
        ax_cpu::virtualization::set_hwi_pending(1);
        ax_cpu::virtualization::GuestInterrupt::new(3)
            .unwrap()
            .pulse();
        assert_eq!(
            (registers::read_guest_csr::<5>() >> 2) & 0xff,
            3,
            "pulsing one HWI must preserve another pending HWI"
        );
        assert!(ax_cpu::virtualization::GuestInterrupt::new(13).is_none());
        ax_cpu::virtualization::set_hwi_pending(0);
        for (value, previous) in [(0x123usize, 0usize), (0x456, 0x123)] {
            let exit = state.run(1, state_address).unwrap();
            assert_eq!(exit.kind, ExitKind::Synchronous);
            assert_eq!((exit.status >> 16) & 0x3f, 0x17, "guest must exit by HVCL");
            assert_eq!(state.context.get_a0(), value);
            assert_eq!(
                state.context.get_a1(),
                previous,
                "guest FP state must be isolated"
            );
            assert_eq!(registers::read_cpu_anchor(), anchor);
            assert_eq!(registers::read_tp(), tp);
            assert!(!interrupt::irqs_enabled());
            // Entering and leaving the guest must not roll the host interrupt
            // mask back to the bind-time snapshot: doing so re-arms a line the
            // host deliberately masked after bind and lets the next clockevent
            // preempt a synchronous check.
            assert!(
                !timer_irq_enabled(),
                "guest exit must not restore a stale host ECFG that re-enables the masked \
                 clockevent line"
            );
            let mut restored = FpuState::default();
            restored.save();
            assert_eq!(restored.fp[0], 0xabc, "guest exit must restore host FP");
            state.context.advance_guest_pc();
        }
        let load_pc = state.context.gcsr_era;
        for (index, page) in [&first, &second].into_iter().enumerate() {
            if index != 0 {
                table.unmap(data_address, 4096).unwrap();
            }
            table
                .map(&MapConfig {
                    vaddr: data_address,
                    paddr: ax_hal::mem::virt_to_phys(VirtAddr::from_usize(
                        (&raw const **page) as usize,
                    )),
                    size: 4096,
                    pte: MappingFlags::READ,
                    allow_huge: false,
                    flush: false,
                })
                .unwrap();
            state.context.gcsr_era = load_pc;
            state.context.sepc = load_pc;
            state.context.x[6] = data_address.as_usize();
            let exit = state.run(1, state_address).unwrap();
            assert_eq!((exit.status >> 16) & 0x3f, 0x17, "guest must exit by HVCL");
            assert_eq!(
                state.context.get_a0(),
                page.0[0] as usize,
                "guest reused a retired nested mapping"
            );
        }
        // A guest with interrupts masked must still be preemptible by the
        // host clockevent. The finite loop exits by HVCL on a broken entry,
        // so failure is an exit-kind assertion rather than a harness timeout.
        let busy_offset = cpu_test_lvz_busy_guest as *const () as usize
            - cpu_test_lvz_guest as *const () as usize;
        state.context.sepc = guest_address.as_usize() + busy_offset;
        state.context.gcsr_era = state.context.sepc;
        state
            .context
            .set_a1((ax_cpu::timer::counter_frequency() / 20) as usize);
        interrupt::set_timer_irq_enabled(true);
        ax_hal::time::set_oneshot_timer(ax_hal::time::monotonic_time_nanos() + 1_000_000);
        let exit = state.run(1, state_address).unwrap();
        ax_hal::time::cancel_oneshot_timer();
        assert_eq!(
            exit.kind,
            ExitKind::Irq,
            "host timer must interrupt a busy guest"
        );
        assert_ne!(exit.status & (1 << 11), 0, "host timer must remain pending");
        assert!(!interrupt::irqs_enabled());

        state.context.sepc = guest_address.as_usize();
        state.context.gcsr_era = guest_address.as_usize();
        let exit = state.run(1, state_address).unwrap();
        assert_eq!(
            exit.kind,
            ExitKind::Synchronous,
            "guest re-entry after the host timer must reach HVCL"
        );
        assert_eq!((exit.status >> 16) & 0x3f, 0x17, "guest must exit by HVCL");
        assert!(!interrupt::irqs_enabled());
        // The host owns ECFG.LIE. A mask changed after the last exit must
        // survive unbind: the binding returns only the borrowed VS field, so
        // releasing the guest cannot undo a host-applied clockevent mask.
        interrupt::set_timer_irq_enabled(false);
        state.unbind().unwrap();
        assert!(
            !timer_irq_enabled(),
            "unbind must not restore a stale host ECFG that re-enables the masked clockevent line"
        );
        cpu.disable().unwrap();
    }
    interrupt::set_timer_irq_enabled(timer_irq);
    original_fp.restore();
    if irq {
        interrupt::enable_irqs();
    }
    std::println!("CPU_GUEST_ENTRY_OK");
}
