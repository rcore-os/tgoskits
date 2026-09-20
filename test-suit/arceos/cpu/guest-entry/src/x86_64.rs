#[path = "fp.rs"]
mod fp;

#[path = "vmx.rs"]
mod vmx;

use ax_cpu::virtualization::{
    Backend, ControlMemory, PerCpu, Readable, SvmControlMemory, SvmControls, SvmExitCode,
    SvmIntercept, VmcbTlbControl, Writeable, set_vmcb_segment,
};

#[path = "../../support/mod.rs"]
mod support;
use support::ControlPages;

unsafe fn read_msr(index: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller selects an implemented host MSR at ring 0.
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") index, out("eax") low, out("edx") high, options(nostack));
    }
    u64::from(low) | (u64::from(high) << 32)
}

fn configure_guest(guest: &mut SvmControls<ControlPages>, code: ax_cpu::PhysAddr, avx: bool) {
    let io_address = guest.io_address();
    let msr_address = guest.msr_address();
    let state = &mut guest.image_mut().state;
    state.cr0.set(0x10 | u64::from(avx));
    state.cr4.set(if avx { (1 << 18) | (1 << 9) } else { 0 });
    state.efer.set(1 << 12);
    state.cs.selector.set(if avx { 8 } else { 0 });
    state.cs.base.set(code.as_usize() as u64);
    state.cs.limit.set(0xffff);
    state.cs.attr.set(0x9b);
    set_vmcb_segment(&mut state.ds, 0, 0x93);
    set_vmcb_segment(&mut state.es, 0, 0x93);
    set_vmcb_segment(&mut state.fs, 0, 0x93);
    set_vmcb_segment(&mut state.gs, 0, 0x93);
    set_vmcb_segment(&mut state.ss, 0, 0x93);
    set_vmcb_segment(&mut state.ldtr, 0, 0x82);
    set_vmcb_segment(&mut state.tr, 0, 0x8b);
    if avx {
        // AVX requires protected mode. Keep 16-bit code, with flat data
        // segments for the address-size-overridden result stores.
        for segment in [
            &mut state.ds,
            &mut state.es,
            &mut state.fs,
            &mut state.gs,
            &mut state.ss,
        ] {
            segment.selector.set(16);
            segment.limit.set(u32::MAX);
            segment.attr.set(0x893);
        }
    }
    state.gdtr.limit.set(0xffff);
    state.idtr.limit.set(0xffff);
    state.dr6.set(0xffff_0ff0);
    state.dr7.set(0x400);
    state.rflags.set(2);
    // SAFETY: IA32_PAT is implemented on this x86_64 CPU.
    state.g_pat.set(unsafe { read_msr(0x277) });
    let control = &mut guest.image_mut().control;
    control.guest_asid.set(1);
    control.tlb_control.set(VmcbTlbControl::FlushAll as u8);
    control.set_intercept(SvmIntercept::Vmrun, true);
    control.set_intercept(SvmIntercept::Vmmcall, true);
    control.set_intercept(SvmIntercept::Shutdown, true);
    control.set_intercept(SvmIntercept::IoioProt, true);
    control.set_intercept(SvmIntercept::MsrProt, true);
    control.iopm_base_pa.set(io_address.as_usize() as u64);
    control.msrpm_base_pa.set(msr_address.as_usize() as u64);
    control.intercept_exceptions.set(u32::MAX);
}

pub fn run() {
    if Backend::detect() == Some(Backend::Vmx) {
        return vmx::run();
    }
    assert_eq!(Backend::detect(), Some(Backend::Svm));
    let code = ControlPages::new(0);
    let mut cpu = PerCpu::new(ControlPages::new(0)).unwrap();
    let memory = SvmControlMemory {
        guest: ControlPages::new(0),
        host: ControlPages::new(0),
        io_permissions: ControlPages::allocate(3),
        msr_permissions: ControlPages::allocate(2),
    };
    // SAFETY: the ArceOS test runs on its initialized ring-0 CPU.
    let layout = unsafe { ax_cpu::virtualization::XstateLayout::current() };
    let pages = layout.byte_len().div_ceil(4096);
    let xstate = ax_cpu::virtualization::GuestXstate::new(
        layout,
        ControlPages::allocate(pages),
        ControlPages::allocate(pages),
    )
    .unwrap();
    // SAFETY: this test owns inactive leases on its initialized ring-0 CPU.
    let mut guest = unsafe {
        ax_cpu::virtualization::Vcpu::new(
            ax_cpu::virtualization::VcpuControlMemory::Svm(memory),
            xstate,
        )
    }
    .unwrap();
    guest
        .svm_controls_mut()
        .unwrap()
        .set_io_intercept(0x6000, false);
    guest
        .svm_controls_mut()
        .unwrap()
        .set_io_range(0x6000, 1, true)
        .unwrap();
    guest
        .svm_controls_mut()
        .unwrap()
        .set_msr_read_intercept(0xc001_1fff, false)
        .unwrap();
    guest
        .svm_controls_mut()
        .unwrap()
        .set_msr_read_intercept(0xc001_1fff, true)
        .unwrap();
    assert_eq!(
        guest
            .svm_controls_mut()
            .unwrap()
            .set_io_range(0xffff, 2, false),
        Err(ax_cpu::virtualization::VirtualizationError::InvalidPortRange)
    );
    assert_eq!(
        guest
            .svm_controls_mut()
            .unwrap()
            .set_msr_read_intercept(0x4000_0000, false),
        Err(ax_cpu::virtualization::VirtualizationError::UnsupportedMsr)
    );
    // Trusted real-mode text modifies GPRs and probes intercepted I/O/MSR
    // operations. It uses no stack, data mapping, or external guest image.
    let avx = guest.extended_state().xcr0() & 4 != 0;
    let clear_ymm = fp::clear_ymm(avx);
    let store_ymm = fp::store_ymm(avx, code.physical_address().as_usize() as u32 + 160);
    let fp_store = (code.physical_address().as_usize() as u32 + 128).to_le_bytes();
    let instructions = [
        0xdbu8,
        0xe3,
        0xd9,
        0xe8, // fninit; fld1
        clear_ymm[0],
        clear_ymm[1],
        clear_ymm[2],
        clear_ymm[3],
        0xb8,
        0x34,
        0x12, // mov ax, 0x1234
        0xb9,
        0x78,
        0x56, // mov cx, 0x5678
        0x0f,
        0x01,
        0xd9, // vmmcall
        0x67,
        0xdd,
        0x1d,
        fp_store[0],
        fp_store[1],
        fp_store[2],
        fp_store[3], // fstp qword [addr32]
        store_ymm[0],
        store_ymm[1],
        store_ymm[2],
        store_ymm[3],
        store_ymm[4],
        store_ymm[5],
        store_ymm[6],
        store_ymm[7],
        store_ymm[8],
        0xbb,
        0x67,
        0x45, // mov bx, 0x4567
        0xba,
        0x00,
        0x60, // mov dx, 0x6000
        0xec, // in al, dx (must be intercepted)
        0x0f,
        0x01,
        0xd9, // vmmcall
        0x66,
        0xb9,
        0xff,
        0x1f,
        0x01,
        0xc0, // mov ecx, 0xc0011fff
        0x0f,
        0x32, // rdmsr (must be intercepted)
        0x0f,
        0x01,
        0xd9, // vmmcall
    ];
    // SAFETY: the exclusive allocated page is mapped WB and is large enough.
    unsafe {
        core::ptr::copy_nonoverlapping(
            instructions.as_ptr(),
            code.virtual_address().as_ptr(),
            instructions.len(),
        );
    }
    configure_guest(
        guest.svm_controls_mut().unwrap(),
        code.physical_address(),
        avx,
    );
    let state = &mut guest.svm_controls_mut().unwrap().image_mut().state;
    let original_efer = state.efer.get();
    let original_cs = state.cs.attr.get();
    state.efer.set(original_efer | (1 << 10));
    state.cs.attr.set(original_cs | (1 << 9));
    assert_eq!(
        guest.execution_mode().unwrap(),
        ax_cpu::virtualization::ExecutionMode::Mode64,
        "VMCB CS.L uses bit 9, unlike the VMCS access-right field"
    );
    let state = &mut guest.svm_controls_mut().unwrap().image_mut().state;
    state.efer.set(original_efer);
    state.cs.attr.set(original_cs);
    let irqs = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    // SAFETY: one CPU, exclusive control pages and trusted bounded guest text.
    // The CPU entry owns the guest FP switch. GIF excludes asynchronous host
    // handlers while guest VMLOAD state is live; CPU assembly restores host GS.
    unsafe {
        let host_gs = read_msr(0xc000_0101);
        let host_fs = read_msr(0xc000_0100);
        cpu.enable().unwrap();
        guest.bind().unwrap();
        // VMRUN interception is mandatory. Hardware must reject the image and
        // the machine window must still restore the host VMLOAD register bank.
        guest
            .svm_controls_mut()
            .unwrap()
            .image_mut()
            .control
            .set_intercept(SvmIntercept::Vmrun, false);
        guest.run().unwrap();
        assert_eq!(
            guest.svm_controls().unwrap().image().exit_info().exit_code,
            Ok(SvmExitCode::Invalid)
        );
        assert_eq!(read_msr(0xc000_0101), host_gs);
        assert_eq!(read_msr(0xc000_0100), host_fs);
        // QEMU writes its current state into the VMCB on this early INVALID
        // exit. Reinitialize the retired lease for a new guest configuration
        // instead of treating that failure image as a resumable guest.
        guest.svm_controls_mut().unwrap().reset_guest_image();
        *guest.registers_mut() = Default::default();
        configure_guest(
            guest.svm_controls_mut().unwrap(),
            code.physical_address(),
            avx,
        );

        for (reason, expected_rip, length) in [
            (SvmExitCode::Vmmcall, 14, 3),
            (SvmExitCode::Ioio, 39, 1),
            (SvmExitCode::Vmmcall, 40, 3),
            (SvmExitCode::Msr, 49, 2),
            (SvmExitCode::Vmmcall, 51, 3),
        ] {
            let host_fp = fp::HostFp::begin(avx);
            guest.run().unwrap();
            host_fp.finish();
            let exit = guest.svm_controls().unwrap().image().exit_info();
            assert_eq!(exit.exit_code, Ok(reason));
            assert_eq!(exit.guest_rip, expected_rip);
            assert_eq!(
                guest.svm_controls().unwrap().image().state.rax.get(),
                0x1234
            );
            assert_eq!(
                guest.registers().rcx,
                if expected_rip < 43 {
                    0x5678
                } else {
                    0xc001_1fff
                }
            );
            if reason == SvmExitCode::Ioio {
                if avx {
                    assert_eq!(
                        core::slice::from_raw_parts(code.virtual_address().as_ptr().add(160), 32),
                        &[0; 32],
                        "guest YMM data must survive reentry"
                    );
                }
                assert_eq!(
                    code.virtual_address()
                        .as_ptr()
                        .add(128)
                        .cast::<u64>()
                        .read(),
                    1.0f64.to_bits(),
                    "guest x87 value must survive reentry"
                );
                assert_eq!(exit.exit_info_1 >> 16 & 0xffff, 0x6000);
            }
            assert_eq!(read_msr(0xc000_0101), host_gs);
            assert_eq!(read_msr(0xc000_0100), host_fs);
            assert!(!ax_cpu::interrupt::irqs_enabled());
            guest
                .svm_controls()
                .unwrap()
                .image()
                .state
                .rip
                .set(expected_rip + length);
        }
        assert_eq!(guest.registers().rbx, 0x4567);
        guest.unbind().unwrap();
        cpu.disable().unwrap();
    }
    drop(
        cpu.into_memory()
            .unwrap_or_else(|_| panic!("CPU must retire its lease")),
    );
    if irqs {
        ax_cpu::interrupt::enable_irqs();
    }
    drop(guest);
    drop(code);
    assert_eq!(
        support::LIVE_PAGES.load(core::sync::atomic::Ordering::Relaxed),
        0
    );
    std::println!("CPU_GUEST_ENTRY_OK");
}
