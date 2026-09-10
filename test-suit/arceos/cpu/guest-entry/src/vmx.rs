use ax_cpu::{
    PhysAddr,
    paging::{EptEntry, EptFlags, EptMemoryType, PageTableEntry},
    virtualization::{
        ControlMemory, PerCpu, VmcsControl32, VmcsControl64, VmcsGuest16, VmcsGuest32, VmcsGuest64,
        VmcsGuestNW, VmxControlMemory, VmxControls, VmxExitReason, VmxInterruptInfo,
        VmxInterruptionType,
    },
};

use super::{
    read_msr,
    support::{ControlPages, LIVE_PAGES},
};

unsafe fn table_entry(page: &ControlPages, index: usize, entry: EptEntry) {
    assert!(index < 512);
    // SAFETY: the exclusive page is aligned, mapped and holds 512 descriptors;
    // the owner has stopped guest execution before every descriptor mutation.
    unsafe {
        page.virtual_address()
            .cast::<EptEntry>()
            .as_ptr()
            .add(index)
            .write(entry)
    };
}

fn configure_guest(controls: &mut VmxControls<ControlPages>, avx: bool) {
    macro_rules! segment {
        ($selector:expr, $base:expr, $limit:expr, $rights:expr, $access:expr) => {{
            controls.write($selector, 0).unwrap();
            controls.write($base, 0).unwrap();
            controls.write($limit, 0xffff).unwrap();
            controls.write($rights, $access).unwrap();
        }};
    }
    segment!(
        VmcsGuest16::CS_SELECTOR,
        VmcsGuestNW::CS_BASE,
        VmcsGuest32::CS_LIMIT,
        VmcsGuest32::CS_ACCESS_RIGHTS,
        0x9b
    );
    segment!(
        VmcsGuest16::SS_SELECTOR,
        VmcsGuestNW::SS_BASE,
        VmcsGuest32::SS_LIMIT,
        VmcsGuest32::SS_ACCESS_RIGHTS,
        0x93
    );
    segment!(
        VmcsGuest16::DS_SELECTOR,
        VmcsGuestNW::DS_BASE,
        VmcsGuest32::DS_LIMIT,
        VmcsGuest32::DS_ACCESS_RIGHTS,
        0x93
    );
    segment!(
        VmcsGuest16::ES_SELECTOR,
        VmcsGuestNW::ES_BASE,
        VmcsGuest32::ES_LIMIT,
        VmcsGuest32::ES_ACCESS_RIGHTS,
        0x93
    );
    segment!(
        VmcsGuest16::FS_SELECTOR,
        VmcsGuestNW::FS_BASE,
        VmcsGuest32::FS_LIMIT,
        VmcsGuest32::FS_ACCESS_RIGHTS,
        0x93
    );
    segment!(
        VmcsGuest16::GS_SELECTOR,
        VmcsGuestNW::GS_BASE,
        VmcsGuest32::GS_LIMIT,
        VmcsGuest32::GS_ACCESS_RIGHTS,
        0x93
    );
    segment!(
        VmcsGuest16::TR_SELECTOR,
        VmcsGuestNW::TR_BASE,
        VmcsGuest32::TR_LIMIT,
        VmcsGuest32::TR_ACCESS_RIGHTS,
        0x8b
    );
    segment!(
        VmcsGuest16::LDTR_SELECTOR,
        VmcsGuestNW::LDTR_BASE,
        VmcsGuest32::LDTR_LIMIT,
        VmcsGuest32::LDTR_ACCESS_RIGHTS,
        0x82
    );
    // SAFETY: the test runs on a VMX CPU and reads implemented capability MSRs.
    let (cr0, cr4, pat) = unsafe {
        (
            (read_msr(0x486) | 0x10) & !((1 << 0) | (1 << 31)),
            read_msr(0x488),
            read_msr(0x277),
        )
    };
    controls
        .write(VmcsGuestNW::CR0, cr0 as usize | usize::from(avx))
        .unwrap();
    controls
        .write(
            VmcsGuestNW::CR4,
            cr4 as usize | if avx { (1 << 18) | (1 << 9) } else { 0 },
        )
        .unwrap();
    controls.write(VmcsGuestNW::RIP, 0x1000).unwrap();
    controls.write(VmcsGuestNW::RFLAGS, 2).unwrap();
    controls.write(VmcsGuestNW::DR7, 0x400).unwrap();
    controls.write(VmcsGuest32::GDTR_LIMIT, 0xffff).unwrap();
    controls.write(VmcsGuest32::IDTR_LIMIT, 0xffff).unwrap();
    controls.write(VmcsGuest64::LINK_PTR, u64::MAX).unwrap();
    controls.write(VmcsGuest64::IA32_PAT, pat).unwrap();
    controls.write(VmcsGuest64::IA32_EFER, 0).unwrap();
    if avx {
        // AVX is not defined in real mode; use protected 16-bit segments.
        controls.write(VmcsGuest16::CS_SELECTOR, 8).unwrap();
        for field in [
            VmcsGuest16::SS_SELECTOR,
            VmcsGuest16::DS_SELECTOR,
            VmcsGuest16::ES_SELECTOR,
            VmcsGuest16::FS_SELECTOR,
            VmcsGuest16::GS_SELECTOR,
        ] {
            controls.write(field, 16).unwrap();
        }
    }
}

pub fn run() {
    let mut entry_cycles = [0u64; 7];
    let lmsw = ax_cpu::virtualization::CrAccessInfo::decode((0xabcd << 16) | (3 << 4));
    assert_eq!(lmsw.lmsw_source_data, 0xabcd, "LMSW operand was truncated");
    // SAFETY: the ArceOS test runs on its initialized ring-0 CPU.
    let layout = unsafe { ax_cpu::virtualization::XstateLayout::current() };
    let pages = layout.byte_len().div_ceil(4096);
    let xstate = ax_cpu::virtualization::GuestXstate::new(
        layout,
        ControlPages::allocate(pages),
        ControlPages::allocate(pages),
    )
    .unwrap();
    let code = ControlPages::new(0);
    let remap_pages = [ControlPages::new(0), ControlPages::new(0)];
    let tables: [ControlPages; 4] = core::array::from_fn(|_| ControlPages::new(0));
    let mut cpu = PerCpu::new(ControlPages::new(0)).unwrap();
    let memory = VmxControlMemory {
        vmcs: ControlPages::new(0),
        io_bitmap_a: ControlPages::new(0),
        io_bitmap_b: ControlPages::new(0),
        msr_bitmap: ControlPages::new(0),
    };
    let avx = xstate.xcr0() & 4 != 0;
    let clear_ymm = super::fp::clear_ymm(avx);
    let store_ymm = super::fp::store_ymm(avx, 0x10a0);
    let instructions = [
        0xdbu8,
        0xe3,
        0xd9,
        0xe8, // fninit; fld1
        clear_ymm[0],
        clear_ymm[1],
        clear_ymm[2],
        clear_ymm[3],
        0x66,
        0xb8,
        0x00,
        0x50,
        0x34,
        0x12, // mov eax, 0x12345000
        0x0f,
        0x22,
        0xd0, // mov cr2, eax
        0x66,
        0xb9,
        0x82,
        0x00,
        0x00,
        0xc0, // mov ecx, IA32_LSTAR
        0x66,
        0xb8,
        0x78,
        0x56,
        0x34,
        0x12, // mov eax, 0x12345678
        0x66,
        0x31,
        0xd2, // xor edx, edx
        0x0f,
        0x30, // wrmsr
        0xb8,
        0x34,
        0x12,
        0xb9,
        0x78,
        0x56,
        0x0f,
        0x01,
        0xc1, // vmcall
        0xdd,
        0x1e,
        0x80,
        0x10, // fstp qword [0x1080]
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
        0x45,
        0x0f,
        0x01,
        0xc1,
        0x0f,
        0x01,
        0xc1,
        0xf4,
    ];
    // SAFETY: all memory is exclusively owned, inactive, mapped and page aligned.
    unsafe {
        core::ptr::copy_nonoverlapping(
            instructions.as_ptr(),
            code.virtual_address().as_ptr(),
            instructions.len(),
        );
        for level in 0..3 {
            table_entry(
                &tables[level],
                0,
                EptEntry::new_table(tables[level + 1].physical_address()),
            );
        }
        table_entry(
            &tables[3],
            1,
            EptEntry::new_page(
                code.physical_address(),
                (EptFlags::READ | EptFlags::WRITE | EptFlags::EXECUTE)
                    .with_memory_type(EptMemoryType::WriteBack),
                false,
            ),
        );
    }
    let irq_enabled = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    // SAFETY: one pinned CPU, IRQs excluded and all control/table/code leases
    // retained. The trusted guest uses only the configured FP and syscall bank.
    unsafe {
        ax_cpu::boot::authorize_vmx().unwrap();
        cpu.enable().unwrap();
        let mut guest = ax_cpu::virtualization::Vcpu::new(
            ax_cpu::virtualization::VcpuControlMemory::Vmx(memory),
            xstate,
        )
        .unwrap();
        guest.bind().unwrap();
        configure_guest(guest.vmx_controls_mut().unwrap(), avx);
        assert_eq!(
            guest.execution_mode().unwrap(),
            if avx {
                ax_cpu::virtualization::ExecutionMode::Protected
            } else {
                ax_cpu::virtualization::ExecutionMode::Real
            },
            "guest mode must not inherit the host EFER"
        );
        use ax_cpu::virtualization::{VmxControl, VmxControlError};
        for (control, field, requested) in [
            (
                VmxControl::PinBased,
                VmcsControl32::PINBASED_EXEC_CONTROLS,
                0,
            ),
            (
                VmxControl::Primary,
                VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS,
                (1 << 31) | (1 << 28),
            ),
            (
                VmxControl::Secondary,
                VmcsControl32::SECONDARY_PROCBASED_EXEC_CONTROLS,
                (1 << 1) | (1 << 7),
            ),
            (
                VmxControl::Exit,
                VmcsControl32::VMEXIT_CONTROLS,
                (1 << 9) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 21),
            ),
            (
                VmxControl::Entry,
                VmcsControl32::VMENTRY_CONTROLS,
                (1 << 14) | (1 << 15),
            ),
        ] {
            let controls = guest.vmx_controls_mut().unwrap();
            controls.initialize_control(control, requested, 0).unwrap();
            assert_eq!(controls.read(field).unwrap() & requested, requested);
        }
        assert_eq!(
            guest
                .vmx_controls_mut()
                .unwrap()
                .update_control(VmxControl::PinBased, 1, 1),
            Err(VmxControlError::ConflictingMasks)
        );
        {
            let controls = guest.vmx_controls_mut().unwrap();
            let original = controls.read(VmcsGuest64::IA32_EFER).unwrap();
            for enable in [false, true] {
                for paging in [false, true] {
                    controls
                        .write(VmcsGuest64::IA32_EFER, if enable { 1 << 8 } else { 0 })
                        .unwrap();
                    controls
                        .synchronize_long_mode(if paging { 1 << 31 } else { 0 })
                        .unwrap();
                    assert_eq!(
                        controls.read(VmcsGuest64::IA32_EFER).unwrap() & (1 << 10) != 0,
                        enable && paging,
                        "LMA requires both LME and guest paging"
                    );
                    assert_eq!(
                        controls.read(VmcsControl32::VMENTRY_CONTROLS).unwrap() & (1 << 9) != 0,
                        enable && paging,
                        "VM-entry mode must track guest LMA"
                    );
                }
            }
            controls.write(VmcsGuest64::IA32_EFER, original).unwrap();
            controls
                .synchronize_long_mode(controls.read(VmcsGuestNW::CR0).unwrap() as u64)
                .unwrap();
        }
        let msr_address = guest.vmx_controls().unwrap().msr_address().as_usize() as u64;
        guest
            .vmx_controls_mut()
            .unwrap()
            .set_msr_write_intercept(0xc000_0082, false)
            .unwrap();
        guest
            .vmx_controls_mut()
            .unwrap()
            .write(VmcsControl64::MSR_BITMAPS_ADDR, msr_address)
            .unwrap();
        let root: PhysAddr = tables[0].physical_address();
        assert_eq!(
            ax_cpu::virtualization::EptPointer::for_current_cpu(root + 1, false),
            Err(ax_cpu::virtualization::VirtualizationError::InvalidRoot),
            "misaligned EPT root must not silently select another address"
        );
        let pointer = ax_cpu::virtualization::EptPointer::for_current_cpu(root, false).unwrap();
        guest
            .vmx_controls_mut()
            .unwrap()
            .write(VmcsControl64::EPTP, pointer.bits())
            .unwrap();
        pointer.invalidate().unwrap();
        assert_eq!(
            ax_cpu::virtualization::invalidate_ept(
                ax_cpu::virtualization::EptInvalidation::SingleContext,
                7,
            ),
            Err(ax_cpu::virtualization::VirtualizationError::InstructionFailed),
            "invalid EPT memory type must report VMfail",
        );
        assert_eq!(
            VmxInterruptInfo::decode(0xaa | (7 << 8) | (1 << 11), Some(123)),
            VmxInterruptInfo {
                vector: 0,
                int_type: VmxInterruptionType::External,
                err_code: None,
                valid: false
            }
        );
        for (iteration, rip) in [(0, 0x1028), (1, 0x103b), (2, 0x103e)] {
            // Rebind once to prove VMLAUNCH after VMCLEAR, then VMRESUME.
            let host_syscall = [
                0xc000_0081,
                0xc000_0082,
                0xc000_0083,
                0xc000_0084,
                0xc000_0102,
            ]
            .map(|index| read_msr(index));
            let host_cr2: u64;
            core::arch::asm!("mov {}, cr2", out(reg) host_cr2, options(nostack));
            let host_fp = super::fp::HostFp::begin(avx);
            let exit = match guest.run().unwrap() {
                ax_cpu::virtualization::Exit::Vmx(exit) => exit,
                _ => unreachable!(),
            };
            host_fp.finish();
            let returned_cr2: u64;
            core::arch::asm!("mov {}, cr2", out(reg) returned_cr2, options(nostack));
            assert_eq!(
                returned_cr2, host_cr2,
                "VM exit must restore the host page-fault address"
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
            assert!(!exit.entry_failure, "invalid guest: {exit:?}");
            assert_eq!(exit.exit_reason, Ok(VmxExitReason::Vmcall));
            assert_eq!(exit.guest_rip, rip);
            assert_eq!(guest.syscall_registers().lstar, 0x12345678);
            assert_eq!(guest.page_fault_address(), 0x12345000);
            assert_eq!(guest.registers().rax, 0x12341234);
            assert_eq!(guest.registers().rcx, 0xc0005678);
            if iteration == 1 {
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
                assert_eq!(guest.registers().rbx, 0x4567);
            }
            guest
                .vmx_controls_mut()
                .unwrap()
                .write(VmcsGuestNW::RIP, rip + 3)
                .unwrap();
            if iteration == 0 {
                guest.unbind().unwrap();
                guest.bind().unwrap();
            }
        }
        // Keep the same VMCS binding and EPTP while replacing a resident leaf.
        // The VM memory owner has quiesced its only guest before retiring a PTE.
        let read_program = [0xa1u8, 0x00, 0x20, 0x0f, 0x01, 0xc1];
        core::ptr::copy_nonoverlapping(
            read_program.as_ptr(),
            code.virtual_address().as_ptr().add(0x200),
            read_program.len(),
        );
        for (page, value) in remap_pages.iter().zip([0x1234u16, 0x5678]) {
            page.virtual_address().as_ptr().cast::<u16>().write(value);
            table_entry(
                &tables[3],
                2,
                EptEntry::new_page(
                    page.physical_address(),
                    EptFlags::READ.with_memory_type(EptMemoryType::WriteBack),
                    false,
                ),
            );
            guest
                .vmx_controls_mut()
                .unwrap()
                .write(VmcsGuestNW::RIP, 0x1200)
                .unwrap();
            let exit = match guest.run().unwrap() {
                ax_cpu::virtualization::Exit::Vmx(exit) => exit,
                _ => unreachable!(),
            };
            assert_eq!(exit.exit_reason, Ok(VmxExitReason::Vmcall));
            assert_eq!(
                guest.registers().rax as u16,
                value,
                "VMRESUME retained an obsolete EPT translation"
            );
        }
        // Cached hardware limits must not cache mutable VMCS operands. A
        // reserved EPTP bit is rejected before attempting another guest entry.
        guest
            .vmx_controls_mut()
            .unwrap()
            .write(VmcsControl64::EPTP, pointer.bits() | (1 << 7))
            .unwrap();
        assert_eq!(
            guest.run().unwrap_err(),
            ax_cpu::virtualization::RunError::Control(
                ax_cpu::virtualization::VirtualizationError::InvalidRoot
            )
        );
        guest
            .vmx_controls_mut()
            .unwrap()
            .write(VmcsControl64::EPTP, pointer.bits())
            .unwrap();
        // Repeated VMCALL at the same RIP measures the complete public entry
        // transaction without changing mappings. Keep every lease and the CPU
        // pin alive; the guest's IF remains clear and it accesses no new memory.
        for sample in &mut entry_cycles {
            let started = ax_cpu::timer::read_counter();
            for _ in 0..512 {
                let exit = match guest.run().unwrap() {
                    ax_cpu::virtualization::Exit::Vmx(exit) => exit,
                    _ => unreachable!(),
                };
                assert_eq!(exit.exit_reason, Ok(VmxExitReason::Vmcall));
            }
            *sample = ax_cpu::timer::read_counter().wrapping_sub(started) / 512;
        }
        guest.unbind().unwrap();
        drop(guest);
        cpu.disable().unwrap();
    }
    drop(
        cpu.into_memory()
            .unwrap_or_else(|_| panic!("CPU did not retire")),
    );
    if irq_enabled {
        ax_cpu::interrupt::enable_irqs();
    }
    drop(tables);
    drop(code);
    drop(remap_pages);
    assert_eq!(LIVE_PAGES.load(core::sync::atomic::Ordering::Relaxed), 0);
    std::println!("CPU_VMX_ENTRY_TSC_CYCLES samples={entry_cycles:?}");
    std::println!("CPU_GUEST_ENTRY_OK");
}
