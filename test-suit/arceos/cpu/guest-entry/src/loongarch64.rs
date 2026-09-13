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
);

unsafe extern "C" {
    fn cpu_test_lvz_guest();
}

fn direct_alias(address: VirtAddr) -> VirtAddr {
    VirtAddr::from_usize(0x9000_0000_0000_0000 | ax_hal::mem::virt_to_phys(address).as_usize())
}

#[repr(C, align(4096))]
struct DataPage([u64; 512]);

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
        state.bind(EntryAddresses::new(entries).unwrap()).unwrap();
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
            assert_eq!((exit.status >> 16) & 0x3f, 0x17);
            assert_eq!(
                state.context.get_a0(),
                page.0[0] as usize,
                "guest reused a retired nested mapping"
            );
        }
        state.unbind().unwrap();
        cpu.disable().unwrap();
    }
    original_fp.restore();
    if irq {
        interrupt::enable_irqs();
    }
    std::println!("CPU_GUEST_ENTRY_OK");
}
