use core::cell::UnsafeCell;

use ax_cpu::{
    interrupt,
    registers::{self, FpState},
    virtualization::{GuestBinding, PerCpu, StateMemory, Vcpu, VirtualizationError, enter_guest},
};

core::arch::global_asm!(
    ".section .text",
    ".balign 4",
    ".global cpu_test_guest",
    "cpu_test_guest:",
    "fmv.x.d a1, f0",
    "li a0, 0x123",
    "fmv.d.x f0, a0",
    "ecall",
    "fmv.x.d a1, f0",
    "li a0, 0x456",
    "fmv.d.x f0, a0",
    "ecall",
    ".option push",
    ".option arch, +v",
    ".balign 4",
    ".global cpu_test_vector_guest",
    "cpu_test_vector_guest:",
    "csrr a3, vstart",
    "csrr a4, vl",
    "csrr a5, vtype",
    "csrr a6, vcsr",
    "csrw vstart, zero",
    "vsetvli t0, zero, e8, m1, ta, ma",
    "vmv.x.s a1, v0",
    "vmv.x.s a2, v31",
    "vmv.v.x v0, a0",
    "addi t0, a0, 1",
    "vmv.v.x v31, t0",
    "li t0, 1",
    "csrw vstart, t0",
    "li t0, 3",
    "csrw vcsr, t0",
    "ecall",
    "j cpu_test_vector_guest",
    ".option pop",
);
unsafe extern "C" {
    fn cpu_test_guest();
    fn cpu_test_vector_guest();
}

#[derive(Debug)]
struct VectorMemory {
    words: UnsafeCell<std::vec::Vec<usize>>,
    offset: usize,
}

impl VectorMemory {
    fn new(bytes: usize, offset: usize) -> Self {
        assert!(offset <= bytes);
        Self {
            words: UnsafeCell::new(
                std::vec![usize::MAX; bytes.div_ceil(core::mem::size_of::<usize>())],
            ),
            offset,
        }
    }
}

impl Clone for VectorMemory {
    fn clone(&self) -> Self {
        // SAFETY: this private !Sync lease cannot be accessed concurrently;
        // guest entry requires the exclusive owning Vcpu borrow.
        Self {
            words: UnsafeCell::new(unsafe { &*self.words.get() }.clone()),
            offset: self.offset,
        }
    }
}

// SAFETY: each Vec owns initialized writable storage. The fixed byte offset
// stays in range and is preserved by deep Clone, as are length, contents and
// the allocator's usize alignment. Forgetting retains the allocation; accessors
// never allocate, block or change the range. UnsafeCell permits the mutable
// buffer pointer under a shared lease borrow and makes the lease !Sync.
unsafe impl StateMemory for VectorMemory {
    fn virtual_address(&self) -> core::ptr::NonNull<u8> {
        core::ptr::NonNull::new(
            // SAFETY: this private !Sync lease excludes concurrent Vec access.
            // as_mut_ptr does not materialize a reference to the register bank.
            unsafe { &mut *self.words.get() }
                .as_mut_ptr()
                .cast::<u8>()
                .wrapping_add(self.offset),
        )
        .unwrap()
    }
    fn byte_len(&self) -> usize {
        // SAFETY: no operation changes this Vec length after construction and
        // the private !Sync lease excludes concurrent access.
        unsafe { &*self.words.get() }.len() * core::mem::size_of::<usize>() - self.offset
    }
}

fn check_owned_vector_state(bytes: usize) {
    // Valid leases can still be unsuitable for a vector bank. Reject both
    // insufficient capacity and a byte-offset range without usize alignment.
    for (size, offset) in [(bytes * 32 - 8, 0), (bytes * 32 + 8, 1)] {
        assert!(matches!(
            Vcpu::with_vector_memory(
                bytes,
                VectorMemory::new(size, offset),
                VectorMemory::new(bytes * 32, 0)
            ),
            Err(VirtualizationError::InvalidVectorState)
        ));
    }
    // Excess capacity must not change the explicit architectural VLEN.
    let mut state = Vcpu::with_vector_memory(
        bytes,
        VectorMemory::new(bytes * 32 + 64, 0),
        VectorMemory::new(bytes * 32 + 64, 0),
    )
    .unwrap();
    assert_eq!(state.vector_register_bytes(), bytes);
    state.initialize_supervisor();
    state.guest_regs.sepc = ax_hal::mem::virt_to_phys(ax_cpu::VirtAddr::from_usize(
        cpu_test_vector_guest as *const () as usize,
    ))
    .as_usize();
    state.vs_csrs.vsstatus = (3 << 13) | (3 << 9);
    let run = |state: &mut Vcpu<VectorMemory>, seed: usize, previous: usize| {
        state.guest_regs.gprs.a0 = seed;
        // SAFETY: the caller holds this single hart's H owner with IRQs off.
        // The trusted guest uses no memory; both vector banks are owned by
        // state and match this CPU. Host vector use is confined to this scope.
        unsafe {
            let status: usize;
            core::arch::asm!(
                ".option push", ".option arch, +v",
                "csrr {status}, sstatus", "li t0, 0x600", "csrs sstatus, t0",
                "vsetvli t0, zero, e16, m1, ta, ma",
                "li t0, 64", "vmv.v.x v0, t0",
                "li t0, 65", "vmv.v.x v31, t0",
                "li t0, 2", "csrw vstart, t0",
                "li t0, 5", "csrw vcsr, t0",
                ".option pop", status = out(reg) status, out("t0") _,
                out("v0") _, out("v31") _, options(nostack),
            );
            let binding = GuestBinding::load(state).unwrap();
            enter_guest(state);
            let (first, last, start, length, kind, control): (
                usize,
                usize,
                usize,
                usize,
                usize,
                usize,
            );
            core::arch::asm!(
                ".option push", ".option arch, +v",
                "csrr {start}, vstart", "csrr {length}, vl",
                "csrr {kind}, vtype", "csrr {control}, vcsr",
                "csrw vstart, zero", "vmv.x.s {first}, v0", "vmv.x.s {last}, v31",
                "csrw sstatus, {status}", ".option pop",
                first = out(reg) first, last = out(reg) last,
                start = out(reg) start, length = out(reg) length,
                kind = out(reg) kind, control = out(reg) control,
                status = in(reg) status, options(nostack),
            );
            binding.unload(state);
            assert_eq!(
                (first, last, start, length, kind, control),
                (64, 65, 2, bytes / 2, 0xc8, 5)
            );
        }
        assert_eq!(state.trap_csrs.scause, 10);
        assert_eq!(state.guest_regs.gprs.a1, previous);
        assert_eq!(
            state.guest_regs.gprs.a2,
            if previous == 0 { 0 } else { previous + 1 }
        );
        if previous != 0 {
            assert_eq!(state.guest_regs.gprs.a3, 1);
            assert_eq!(state.guest_regs.gprs.a4, bytes);
            assert_eq!(state.guest_regs.gprs.a5, 0xc0);
            assert_eq!(state.guest_regs.gprs.a6, 3);
        }
        state.guest_regs.sepc += 4;
    };
    run(&mut state, 7, 0);
    let mut cloned = state.clone();
    run(&mut state, 11, 7);
    run(&mut cloned, 19, 7);
    run(&mut state, 23, 11);
    std::println!("CPU_GUEST_VECTOR_OK");
}

pub fn run() {
    assert!(ax_cpu::capability::has_hypervisor_extension());
    let irq = interrupt::irqs_enabled();
    interrupt::disable_irqs();
    let tp = registers::read_tp();
    let scratch = registers::read_sscratch();
    let mut cpu = PerCpu::new();
    // SAFETY: this test owns the single hart with IRQs disabled and no guest.
    unsafe { cpu.enable().unwrap() };
    assert!(matches!(cpu.max_guest_page_table_levels(), 3 | 4));
    assert_eq!(
        unsafe { cpu.enable() },
        Err(VirtualizationError::AlreadyEnabled)
    );
    let mut state: Vcpu = Vcpu::default();
    state.initialize_supervisor();
    state.guest_regs.sepc = ax_hal::mem::virt_to_phys(ax_cpu::VirtAddr::from_usize(
        cpu_test_guest as *const () as usize,
    ))
    .as_usize();
    state.vs_csrs.vsstatus = 3 << 13;
    state.vs_csrs.vsscratch = 0xabc;
    if cfg!(feature = "expect-no-sstc") {
        let before: usize;
        let after: usize;
        // SAFETY: the current enabled H owner is pinned, with IRQs disabled.
        unsafe {
            core::arch::asm!("csrr {}, henvcfg", out(reg) before, options(nostack));
            assert!(matches!(
                GuestBinding::load(&state),
                Err(VirtualizationError::UnsupportedTimer)
            ));
            core::arch::asm!("csrr {}, henvcfg", out(reg) after, options(nostack));
            assert_eq!(after, before, "failed binding must restore HENVCFG");
            cpu.disable().unwrap();
        }
        if irq {
            interrupt::enable_irqs();
        }
        std::println!("CPU_GUEST_TIMER_UNSUPPORTED_OK");
        return;
    }
    let saved_status = registers::sstatus::read();
    let mut original_fp = FpState::default();
    let saved_sie: usize;
    // SAFETY: this one-hart test owns an IRQ-disabled machine window. The
    // guest is fixed trusted code, accesses no memory and exits by ecall.
    unsafe {
        registers::sstatus::set_fs(registers::FS::Dirty);
        original_fp.save();
        core::arch::asm!("csrrw {}, sie, zero", out(reg) saved_sie, options(nostack));
        // No address translation or delegation is needed by this trusted
        // straight-line guest. There is no live VM or vCPU on this test hart.
        core::arch::asm!("csrw hgatp, zero", "csrw vsatp, zero",
            "csrw vsstatus, {guest_status}", "csrw hedeleg, zero", "csrw hideleg, zero", "csrw hvip, zero",
            ".option push", ".option arch, +h", "hfence.gvma", ".option pop", guest_status = in(reg) (3usize << 13), options(nostack));
        for (expected, previous) in [(0x123usize, 0), (0x456, 0x123)] {
            let host_vsscratch: usize;
            core::arch::asm!("csrr {}, vsscratch", out(reg) host_vsscratch, options(nostack));
            // An unbound controller update changes only the saved guest image.
            state.set_interrupt_pending(ax_cpu::virtualization::GuestInterrupt::External, true);
            let pending_before_load: usize;
            core::arch::asm!("csrr {}, hvip", out(reg) pending_before_load, options(nostack));
            assert_eq!(pending_before_load & (1 << 10), 0);
            let mut binding = GuestBinding::load(&state).unwrap();
            let loaded_pending: usize;
            core::arch::asm!("csrr {}, hvip", out(reg) loaded_pending, options(nostack));
            assert_ne!(loaded_pending & (1 << 10), 0);
            state.set_interrupt_pending(ax_cpu::virtualization::GuestInterrupt::External, false);
            binding.sync_interrupt(&state, ax_cpu::virtualization::GuestInterrupt::External);
            state.set_interrupt_pending(ax_cpu::virtualization::GuestInterrupt::Software, true);
            core::arch::asm!("csrs hvip, {}", in(reg) (1usize << 6), options(nostack));
            binding.sync_interrupt(&state, ax_cpu::virtualization::GuestInterrupt::Software);
            let pending: usize;
            core::arch::asm!("csrr {}, hvip", out(reg) pending, options(nostack));
            assert_ne!(
                pending & (1 << 6),
                0,
                "software IPI update must preserve timer pending"
            );
            state.set_interrupt_pending(ax_cpu::virtualization::GuestInterrupt::Software, false);
            binding.sync_interrupt(&state, ax_cpu::virtualization::GuestInterrupt::Software);
            core::arch::asm!("csrc hvip, {}", in(reg) (1usize << 6), options(nostack));
            let fetched = ax_cpu::virtualization::fetch_guest_instruction(
                state.guest_regs.sepc.into(),
                ax_cpu::virtualization::GuestPrivilege::Supervisor,
            )
            .unwrap();
            assert_eq!(
                fetched & 3,
                3,
                "the test guest begins with a 32-bit FP instruction"
            );
            let fault = ax_cpu::virtualization::fetch_guest_instruction(
                0usize.into(),
                ax_cpu::virtualization::GuestPrivilege::Supervisor,
            )
            .expect_err("unmapped guest physical memory must fault");
            assert_eq!(
                fault.scause, 1,
                "QEMU reports instruction access fault for unmapped HLVX"
            );
            assert_eq!(fault.stval, 0);
            let host_f0: usize;
            core::arch::asm!("fmv.d.x f0, {}", in(reg) 0x789usize, options(nostack));
            enter_guest(&mut state);
            // Simulate a later trap overwriting the live CSR bank. The exit
            // snapshot must have been made durable before returning to us.
            core::arch::asm!("csrw scause, zero", options(nostack));
            core::arch::asm!("fmv.x.d {}, f0", out(reg) host_f0, options(nostack));
            assert_eq!(state.trap_csrs.scause, 10);
            assert_eq!(state.guest_regs.gprs.a0, expected);
            assert_eq!(state.guest_regs.gprs.a1, previous);
            assert_eq!(registers::read_tp(), tp);
            assert_eq!(registers::read_sscratch(), scratch);
            assert!(!interrupt::irqs_enabled());
            assert_eq!(host_f0, 0x789, "guest FP must not leak into host");
            binding.unload(&mut state);
            let restored: usize;
            core::arch::asm!("csrr {}, vsscratch", out(reg) restored, options(nostack));
            assert_eq!(restored, host_vsscratch);
            state.guest_regs.sepc += 4;
        }
        let host_vsscratch: usize;
        core::arch::asm!("csrr {}, vsscratch", out(reg) host_vsscratch, options(nostack));
        let aborted: Result<(), ()> = {
            let _binding = GuestBinding::load(&state).unwrap();
            Err(())
        };
        assert!(aborted.is_err());
        let restored: usize;
        core::arch::asm!("csrr {}, vsscratch", out(reg) restored, options(nostack));
        assert_eq!(
            restored, host_vsscratch,
            "aborted binding restores the host"
        );
        let vector_bytes = ax_cpu::capability::vector_register_bytes();
        assert!(
            cfg!(feature = "sstc") || vector_bytes.is_some(),
            "the default guest-entry configuration must exercise vector state"
        );
        if let Some(bytes) = vector_bytes {
            check_owned_vector_state(bytes);
        }
        core::arch::asm!("csrw sie, {}", in(reg) saved_sie, options(nostack));
        original_fp.restore();
        registers::sstatus::set_fs(saved_status.fs());
        cpu.disable().unwrap();
        assert!(!cpu.is_enabled());
        assert_eq!(cpu.disable(), Err(VirtualizationError::NotEnabled));
    }
    if irq {
        interrupt::enable_irqs();
    }
    std::println!("CPU_GUEST_ENTRY_OK");
}
