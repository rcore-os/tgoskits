use core::sync::atomic::Ordering;

use ax_cpu::virtualization::{Backend, ControlMemory, PerCpu, VirtualizationError};
#[path = "../../support/mod.rs"]
mod support;
use support::{ControlPages, LIVE_PAGES};

unsafe fn read_msr(index: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller selects an implemented MSR after backend discovery.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") index, out("eax") low, out("edx") high, options(nostack));
    }
    u64::from(low) | (u64::from(high) << 32)
}

unsafe fn write_msr(index: u32, value: u64) {
    // SAFETY: the test owns this CPU and supplies an implemented, valid MSR value.
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") index, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack));
    }
}

pub fn run() {
    let backend = Backend::detect().expect("test requires VMX or SVM");
    assert_eq!(
        backend,
        if cfg!(feature = "expect-svm") {
            Backend::Svm
        } else {
            Backend::Vmx
        }
    );
    assert!(matches!(
        PerCpu::new(ControlPages::new(1)),
        Err(VirtualizationError::InvalidControlMemory)
    ));
    assert_eq!(LIVE_PAGES.load(Ordering::Relaxed), 0);
    // SAFETY: the ArceOS test runs on its initialized ring-0 CPU.
    let layout = unsafe { ax_cpu::virtualization::XstateLayout::current() };
    let pages = layout.byte_len().div_ceil(4096);
    let mut xstate = ax_cpu::virtualization::GuestXstate::new(
        layout,
        ControlPages::allocate(pages),
        ControlPages::allocate(pages),
    )
    .unwrap();
    let previous_hsave_page = ControlPages::new(0);
    let mut cpu = PerCpu::new(ControlPages::new(0)).unwrap();
    assert!(!cpu.is_enabled());
    let vmcs_memory = (backend == Backend::Vmx).then(|| ax_cpu::virtualization::VmxControlMemory {
        vmcs: ControlPages::new(0),
        io_bitmap_a: ControlPages::new(0),
        io_bitmap_b: ControlPages::new(0),
        msr_bitmap: ControlPages::new(0),
    });
    let invalid_vmcs_memory =
        (backend == Backend::Vmx).then(|| ax_cpu::virtualization::VmxControlMemory {
            vmcs: ControlPages::new(0),
            io_bitmap_a: ControlPages::new(1),
            io_bitmap_b: ControlPages::new(0),
            msr_bitmap: ControlPages::new(0),
        });
    let vmcb_memory = (backend == Backend::Svm).then(|| ControlPages::new(0));
    let irqs = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let cr0: u64;
    let cr4: u64;
    // SAFETY: one CPU, IRQs masked, no guests. The old SVM HSAVE address points
    // to a retained allocated page, while the CPU object owns a different page.
    unsafe {
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nostack));
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nostack));
        let old_hsave = if backend == Backend::Svm {
            read_msr(0xc001_0117)
        } else {
            0
        };
        let old_efer = read_msr(0xc000_0080);
        if backend == Backend::Svm {
            write_msr(
                0xc001_0117,
                previous_hsave_page.physical_address().as_usize() as u64,
            );
        } else {
            ax_cpu::boot::authorize_vmx().unwrap();
        }
        assert_eq!(cpu.disable(), Err(VirtualizationError::NotEnabled));
        cpu.enable().unwrap();
        assert!(cpu.is_enabled());
        assert_eq!(cpu.enable(), Err(VirtualizationError::AlreadyEnabled));
        cpu = match cpu.into_memory() {
            Err(active) => active,
            Ok(_) => panic!("active CPU must retain its hardware memory"),
        };
        if let Some(memory) = invalid_vmcs_memory {
            assert!(matches!(
                ax_cpu::virtualization::VmxControls::new(memory),
                Err(VirtualizationError::InvalidControlMemory)
            ));
        }
        assert_eq!(
            LIVE_PAGES.load(Ordering::Relaxed),
            (if backend == Backend::Vmx { 6 } else { 3 }) + 2 * pages
        );
        if let Some(memory) = vmcs_memory {
            use ax_cpu::virtualization::{VmcsGuestNW, VmxControls};
            let mut vmcs = VmxControls::new(memory).unwrap();
            assert_eq!(
                vmcs.read(VmcsGuestNW::RIP),
                Err(VirtualizationError::NotEnabled)
            );
            vmcs.bind().unwrap();
            let mut entry = ax_cpu::virtualization::VmxEntryContext::default();
            vmcs.write(
                ax_cpu::virtualization::VmcsHostNW::RSP,
                entry.host_stack_slot().as_usize(),
            )
            .unwrap();
            vmcs.write(
                ax_cpu::virtualization::VmcsHostNW::RIP,
                ax_cpu::virtualization::VmxEntryContext::exit_address().as_usize(),
            )
            .unwrap();
            // An otherwise zero VMCS cannot enter a guest. The rejection must
            // return through the original host stack, without panicking there.
            let host_syscall = [
                0xc000_0081,
                0xc000_0082,
                0xc000_0083,
                0xc000_0084,
                0xc000_0102,
            ]
            .map(|index| read_msr(index));
            assert_eq!(
                entry.launch(&mut xstate),
                Err(ax_cpu::virtualization::VmxEntryFailure::Valid)
            );
            assert_eq!(
                host_syscall,
                [
                    0xc000_0081,
                    0xc000_0082,
                    0xc000_0083,
                    0xc000_0084,
                    0xc000_0102
                ]
                .map(|index| read_msr(index))
            );
            assert_ne!(
                vmcs.read(ax_cpu::virtualization::VmcsReadOnly32::VM_INSTRUCTION_ERROR)
                    .unwrap(),
                0
            );
            vmcs.write(VmcsGuestNW::RIP, 0x12345).unwrap();
            assert_eq!(vmcs.read(VmcsGuestNW::RIP).unwrap(), 0x12345);
            assert_eq!(vmcs.bind(), Err(VirtualizationError::AlreadyEnabled));
            vmcs.unbind().unwrap();
            assert_eq!(
                vmcs.read(VmcsGuestNW::RIP),
                Err(VirtualizationError::NotEnabled)
            );
            vmcs.bind().unwrap();
            assert_eq!(
                vmcs.read(VmcsGuestNW::RIP).unwrap(),
                0x12345,
                "VMCS fields must survive unbind and rebind"
            );
            vmcs.unbind().unwrap();
            assert!(!vmcs.vmcs().is_bound());
            drop(vmcs);
            assert_eq!(
                LIVE_PAGES.load(Ordering::Relaxed),
                2 + 2 * pages,
                "retired VMCS must release all four control pages"
            );
        }
        if let Some(memory) = vmcb_memory {
            use ax_cpu::virtualization::{Readable, Vmcb};
            let mut vmcb = Vmcb::new(memory).unwrap();
            vmcb.save_current_state();
            let state = &vmcb.image().state;
            assert_eq!(state.star.get(), read_msr(0xc000_0081));
            assert_eq!(state.lstar.get(), read_msr(0xc000_0082));
            assert_eq!(state.cstar.get(), read_msr(0xc000_0083));
            assert_eq!(state.sfmask.get(), read_msr(0xc000_0084));
            assert_eq!(state.kernel_gs_base.get(), read_msr(0xc000_0102));
            drop(vmcb.into_memory());
        }
        cpu.disable().unwrap();
        assert!(!cpu.is_enabled());
        assert_eq!(cpu.disable(), Err(VirtualizationError::NotEnabled));
        let restored_cr0: u64;
        let restored_cr4: u64;
        core::arch::asm!("mov {}, cr0", out(reg) restored_cr0, options(nostack));
        core::arch::asm!("mov {}, cr4", out(reg) restored_cr4, options(nostack));
        assert_eq!((restored_cr0, restored_cr4), (cr0, cr4));
        assert_eq!(read_msr(0xc000_0080), old_efer);
        if backend == Backend::Svm {
            let actual = read_msr(0xc001_0117);
            write_msr(0xc001_0117, old_hsave);
            assert_eq!(
                actual,
                previous_hsave_page.physical_address().as_usize() as u64,
                "SVM shutdown must restore the original HSAVE pointer"
            );
        }
    }
    let lease = cpu
        .into_memory()
        .unwrap_or_else(|_| panic!("retired CPU must return its lease"));
    drop(lease);
    drop(previous_hsave_page);
    drop(xstate);
    assert_eq!(LIVE_PAGES.load(Ordering::Relaxed), 0);
    if irqs {
        ax_cpu::interrupt::enable_irqs();
    }
    std::println!("CPU_VIRTUALIZATION_LIFECYCLE_OK");
}
