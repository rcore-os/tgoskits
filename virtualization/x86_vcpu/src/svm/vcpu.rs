use alloc::collections::VecDeque;
use core::{
    arch::asm,
    fmt::{Debug, Formatter, Result as FmtResult},
    mem::size_of,
};

use ax_errno::{AxResult, ax_err, ax_err_type};
use ax_memory_addr::AddrRange;
use axdevice_base::{BaseDeviceOps, SysRegAddrRange};
use axvm_types::{
    AccessWidth, GuestPhysAddr, GuestVirtAddr, HostPhysAddr, MappingFlags, NestedPageFaultInfo,
    NestedPagingConfig, Port, SysRegAddr, VCpuId, VMId, VmArchVcpuOps, VmExit,
};
use bit_field::BitField;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use x86::controlregs::Xcr0;
use x86_64::registers::{
    control::{Cr0Flags, Cr4Flags, EferFlags},
    rflags::RFlags,
};
use x86_vlapic::EmulatedLocalApic;

use super::{
    definitions::{SvmExitCode, SvmIntercept},
    flags::{InterruptType, VmcbIntInfo},
    structs::{IOPm, MSRPm, VmcbFrame},
    vmcb::{InterceptCrRw, InterceptExceptions, NestedCtl, VmcbTlbControl, set_vmcb_segment},
};
use crate::{
    X86VCpuCreateConfig, X86VCpuSetupConfig, msr::Msr, regs::GeneralRegisters,
    restore_host_interrupt_flag, x86_real_mode_entry_state, xstate::XState,
};

const QEMU_EXIT_PORT: u16 = 0x604;
const QEMU_EXIT_MAGIC: u64 = 0x2000;
const X86_PIT_PORT_BASE: u16 = 0x40;
const X86_PIT_PORT_COUNT: u32 = 4;
const X86_PIT_SPEAKER_PORT: u16 = 0x61;
const X86_COM1_PORT_BASE: u16 = 0x3f8;
const X86_COM1_PORT_COUNT: u32 = 8;
const X86_IOAPIC_BASE: usize = 0xfec0_0000;
const X86_IOAPIC_SIZE: usize = 0x1000;
const X86_LOCAL_APIC_BASE: usize = 0xfee0_0000;
const X86_LOCAL_APIC_SIZE: usize = 0x1000;
const X86_LOCAL_APIC_EOI_OFFSET: usize = 0xb0;

const APIC_BASE_MSR: u32 = 0x1b;
const IA32_UMWAIT_CONTROL: u32 = 0xe1;
const AMD64_DE_CFG: u32 = 0xc001_1029;

const EFER_SVME: u64 = 1 << 12;
const EFER_LMA: u64 = 1 << 10;
const EFER_LME: u64 = 1 << 8;
const CR0_PG: u64 = 1 << 31;
const CR0_PE: u64 = 1 << 0;
// Keep the first SVM Linux guest model conservative. These optional CR4
// features are not required by the smoke path and can make nested SVM VMRUN
// validation fail on some hosted AMD/KVM runners when exposed directly from
// the host CPU model.
const CR4_UMIP: u64 = 1 << 11;
const CR4_LA57: u64 = 1 << 12;
const CR4_FSGSBASE: u64 = 1 << 16;
const CR4_PCIDE: u64 = 1 << 17;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;
const CR4_PKE: u64 = 1 << 22;
const CR4_CET: u64 = 1 << 23;
const CR4_PKS: u64 = 1 << 24;
const SVM_UNSUPPORTED_GUEST_CR4: u64 = CR4_UMIP
    | CR4_LA57
    | CR4_FSGSBASE
    | CR4_PCIDE
    | CR4_SMEP
    | CR4_SMAP
    | CR4_PKE
    | CR4_CET
    | CR4_PKS;
const X2APIC_MSR_BASE: u32 = 0x800;
// Match the current VMX/vLAPIC path, which handles x2APIC register offsets 0x00..=0x3f.
const X2APIC_MSR_END: u32 = 0x83f;
const X2APIC_EOI_MSR: u32 = X2APIC_MSR_BASE + 0xb;
const SVM_INT_CTL_V_IRQ: u32 = 1 << 8;
const SVM_INT_CTL_V_INTR_PRIO_SHIFT: u32 = 16;
const SVM_INT_CTL_V_INTR_PRIO_MASK: u32 = 0xf << SVM_INT_CTL_V_INTR_PRIO_SHIFT;
const SVM_INT_CTL_V_IGN_TPR: u32 = 1 << 20;
const SVM_INT_CTL_V_INTR_MASKING: u32 = 1 << 24;
const SVM_INT_CTL_V_IRQ_INJECTION_BITS: u32 =
    SVM_INT_CTL_V_IRQ | SVM_INT_CTL_V_INTR_PRIO_MASK | SVM_INT_CTL_V_IGN_TPR;
const SVM_INT_STATE_INTERRUPT_SHADOW: u32 = 1 << 0;

macro_rules! save_regs_no_rax {
    () => {
        "
        push r15
        push r14
        push r13
        push r12
        push r11
        push r10
        push r9
        push r8
        push rdi
        push rsi
        push rbp
        sub rsp, 8
        push rbx
        push rdx
        push rcx
        sub rsp, 8"
    };
}

macro_rules! restore_regs_no_rax {
    () => {
        "
        add rsp, 8
        pop rcx
        pop rdx
        pop rbx
        add rsp, 8
        pop rbp
        pop rsi
        pop rdi
        pop r8
        pop r9
        pop r10
        pop r11
        pop r12
        pop r13
        pop r14
        pop r15"
    };
}

#[derive(PartialEq, Eq, Debug)]
pub enum VmCpuMode {
    Real,
    Protected,
    Compatibility,
    Mode64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingEvent {
    vector: u8,
    err_code: Option<u32>,
    level_triggered: bool,
}

/// Host save area used to restore CPU state touched by SVM VMLOAD/VMSAVE.
pub struct VmLoadSaveStates {
    vmcb: VmcbFrame,
}

impl VmLoadSaveStates {
    pub fn new() -> AxResult<Self> {
        Ok(Self {
            vmcb: VmcbFrame::new()?,
        })
    }

    pub fn save(&mut self) {
        unsafe {
            let _ = super::instructions::vmsave(self.vmcb.phys_addr().as_usize() as u64);
        }
    }

    pub fn load(&self) {
        unsafe {
            let _ = super::instructions::vmload(self.vmcb.phys_addr().as_usize() as u64);
        }
    }
}

/// AMD SVM vCPU implementation backed by a VMCB, I/O permission map and MSR
/// permission map.
#[repr(C)]
pub struct SvmVcpu {
    // The order of `guest_regs`, `host_stack_top`, and `host_rflags` is
    // mandatory. They must be the first three fields. If you want to change
    // the order or the type of these fields, you must also change the assembly
    // in this file.
    /// Guest general-purpose registers.
    guest_regs: GeneralRegisters,
    // Used by `svm_run()` assembly; keep immediately after `guest_regs`.
    host_stack_top: u64,
    /// Host RFLAGS captured immediately before VM entry.
    host_rflags: u64,

    // The order of the following fields is not mandatory.
    /// Whether this VCPU has entered the guest at least once.
    launched: bool,
    /// The guest entry point.
    entry: Option<GuestPhysAddr>,
    /// The nested page table root address.
    npt_root: Option<HostPhysAddr>,
    /// The guest VMCB.
    vmcb: VmcbFrame,
    /// Host state saved with VMSAVE and restored with VMLOAD.
    load_save_states: VmLoadSaveStates,
    /// The I/O permission map used by SVM I/O intercepts.
    iopm: IOPm,
    /// The MSR permission map used by SVM MSR intercepts.
    msrpm: MSRPm,
    /// Pending events to be injected to the guest.
    pending_events: VecDeque<PendingEvent>,
    /// Event handed to EVENTINJ for the current VMRUN and awaiting completion.
    injecting_event: Option<PendingEvent>,
    /// Emulated Local APIC for x2APIC MSR accesses.
    vlapic: EmulatedLocalApic,
    /// The XState of the VCpu. Both host and guest.
    xstate: XState,
}

impl SvmVcpu {
    fn create(vm_id: VMId, vcpu_id: VCpuId) -> AxResult<Self> {
        let vcpu = Self {
            guest_regs: GeneralRegisters::default(),
            host_stack_top: 0,
            host_rflags: 0,
            launched: false,
            entry: None,
            npt_root: None,
            vmcb: VmcbFrame::new()?,
            load_save_states: VmLoadSaveStates::new()?,
            iopm: IOPm::passthrough_all()?,
            msrpm: MSRPm::passthrough_all()?,
            pending_events: VecDeque::with_capacity(8),
            injecting_event: None,
            vlapic: EmulatedLocalApic::new(vm_id, vcpu_id),
            xstate: XState::new(),
        };
        info!("[HV] created SvmVcpu(vmcb: {:#x})", vcpu.vmcb.phys_addr());
        Ok(vcpu)
    }

    fn setup_vmcb(
        &mut self,
        entry: GuestPhysAddr,
        npt_root: HostPhysAddr,
        config: X86VCpuSetupConfig,
    ) -> AxResult {
        self.setup_io_bitmap(config)?;
        self.setup_msr_bitmap()?;
        self.setup_vmcb_guest(entry)?;
        self.setup_vmcb_control(npt_root)
    }

    fn setup_vmcb_guest(&mut self, entry: GuestPhysAddr) -> AxResult {
        let entry_state = x86_real_mode_entry_state(entry);
        let cr0_val =
            Cr0Flags::NOT_WRITE_THROUGH | Cr0Flags::CACHE_DISABLE | Cr0Flags::EXTENSION_TYPE;
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        let state = &mut vmcb.state;

        state.cr0.set(cr0_val.bits());
        state.cr3.set(0);
        // CR4 is initialized to zero here which is always a subset of
        // SVM_UNSUPPORTED_GUEST_CR4. If a non-zero CR4 baseline is ever
        // needed, apply the mask: value & !SVM_UNSUPPORTED_GUEST_CR4.
        // handle_cr_write(4) already masks unsupported bits on every
        // guest CR4 write.
        state.cr4.set(0);

        state.cs.selector.set(entry_state.cs_selector);
        state.cs.base.set(entry_state.cs_base as u64);
        state.cs.limit.set(0xffff);
        state.cs.attr.set(0x9b);

        set_vmcb_segment(&mut state.ds, 0, 0x93);
        set_vmcb_segment(&mut state.es, 0, 0x93);
        set_vmcb_segment(&mut state.fs, 0, 0x93);
        set_vmcb_segment(&mut state.gs, 0, 0x93);
        set_vmcb_segment(&mut state.ss, 0, 0x93);
        set_vmcb_segment(&mut state.ldtr, 0, 0x82);
        set_vmcb_segment(&mut state.tr, 0, 0x8b);

        state.gdtr.base.set(0);
        state.gdtr.limit.set(0xffff);
        state.idtr.base.set(0);
        state.idtr.limit.set(0xffff);

        state.dr7.set(0x400);
        state.dr6.set(0xffff0ff0);
        state.rflags.set(0x2);
        state.rip.set(entry_state.rip as u64);
        state.rsp.set(0);
        state.efer.set(EFER_SVME);
        state.g_pat.set(Msr::IA32_PAT.read());

        Ok(())
    }

    fn setup_vmcb_control(&mut self, npt_root: HostPhysAddr) -> AxResult {
        let control = &mut unsafe { self.vmcb.as_vmcb() }.control;

        control.nested_ctl.modify(NestedCtl::NP_ENABLE::SET);
        control.guest_asid.set(1);
        control.nested_cr3.set(npt_root.as_usize() as u64);
        enable_virtual_interrupt_masking_control(control);
        control.clean_bits.set(0);
        control
            .tlb_control
            .modify(VmcbTlbControl::CONTROL::FlushGuestTlb);
        control.intercept_cr.modify(
            InterceptCrRw::WRITE_CR0::SET
                + InterceptCrRw::WRITE_CR3::SET
                + InterceptCrRw::WRITE_CR4::SET,
        );
        // Match the VMX path: let the guest handle normal exceptions itself,
        // while keeping #UD intercepted for unsupported instruction handling.
        control
            .intercept_exceptions
            .modify(InterceptExceptions::UD::SET);

        for intercept in [
            SvmIntercept::INTR,
            SvmIntercept::NMI,
            SvmIntercept::RDTSC,
            SvmIntercept::CPUID,
            SvmIntercept::PAUSE,
            SvmIntercept::HLT,
            SvmIntercept::IOIO_PROT,
            SvmIntercept::MSR_PROT,
            SvmIntercept::SHUTDOWN,
            SvmIntercept::VMRUN,
            SvmIntercept::VMMCALL,
            SvmIntercept::VMLOAD,
            SvmIntercept::VMSAVE,
            SvmIntercept::STGI,
            SvmIntercept::CLGI,
            SvmIntercept::SKINIT,
            SvmIntercept::XSETBV,
        ] {
            control.set_intercept(intercept);
        }

        control
            .iopm_base_pa
            .set(self.iopm.phys_addr().as_usize() as u64);
        control
            .msrpm_base_pa
            .set(self.msrpm.phys_addr().as_usize() as u64);

        Ok(())
    }

    fn setup_msr_bitmap(&mut self) -> AxResult {
        // Keep APIC state in the emulated local APIC instead of exposing the host APIC MSR.
        self.msrpm.set_read_intercept(APIC_BASE_MSR, true);
        self.msrpm.set_write_intercept(APIC_BASE_MSR, true);
        // Keep EFER under software control so the guest never observes or
        // clears the host-required SVME bit stored in the VMCB.
        self.msrpm.set_read_intercept(Msr::IA32_EFER as u32, true);
        self.msrpm.set_write_intercept(Msr::IA32_EFER as u32, true);
        // Match VMX's Linux direct-boot path: UMWAIT and AMD64_DE_CFG are
        // handled in software so guest probes do not leak host-specific state.
        self.msrpm.set_read_intercept(IA32_UMWAIT_CONTROL, true);
        self.msrpm.set_write_intercept(IA32_UMWAIT_CONTROL, true);
        self.msrpm.set_read_intercept(AMD64_DE_CFG, true);
        self.msrpm.set_write_intercept(AMD64_DE_CFG, true);
        // Route x2APIC MSRs through the emulated local APIC instead of the host APIC.
        for msr in X2APIC_MSR_BASE..=X2APIC_MSR_END {
            self.msrpm.set_read_intercept(msr, true);
            self.msrpm.set_write_intercept(msr, true);
        }
        Ok(())
    }

    fn setup_io_bitmap(&mut self, config: X86VCpuSetupConfig) -> AxResult {
        // This port is part of the x86 QEMU test contract: 0x604 reports test completion.
        self.iopm
            .set_intercept_of_range(QEMU_EXIT_PORT as _, 2, true);
        self.iopm
            .set_intercept_of_range(X86_PIT_PORT_BASE as u32, X86_PIT_PORT_COUNT, true);
        self.iopm.set_intercept(X86_PIT_SPEAKER_PORT as u32, true);
        if config.emulate_com1 {
            self.iopm
                .set_intercept_of_range(X86_COM1_PORT_BASE as u32, X86_COM1_PORT_COUNT, true);
        }
        for range in config.passthrough_port_ranges() {
            self.iopm
                .set_intercept_of_range(range.base as u32, range.length as u32, true);
        }
        Ok(())
    }

    fn bind_to_current_processor(&self) -> AxResult {
        Ok(())
    }

    fn unbind_from_current_processor(&self) -> AxResult {
        Ok(())
    }

    pub fn get_cpu_mode(&self) -> VmCpuMode {
        let vmcb = unsafe { self.vmcb.as_vmcb_ref() };
        let efer = vmcb.state.efer.get();
        let cs_attr = vmcb.state.cs.attr.get();
        let cr0 = vmcb.state.cr0.get();

        if efer & EFER_LMA != 0 {
            if cs_attr & (1 << 13) != 0 {
                VmCpuMode::Mode64
            } else {
                VmCpuMode::Compatibility
            }
        } else if cr0 & CR0_PE != 0 {
            VmCpuMode::Protected
        } else {
            VmCpuMode::Real
        }
    }

    pub fn exit_info(&self) -> AxResult<super::vmcb::SvmExitInfo> {
        unsafe { self.vmcb.as_vmcb_ref().exit_info() }
    }

    pub fn nested_page_fault_info(&self) -> AxResult<NestedPageFaultInfo> {
        let info = self.exit_info()?;
        // For SVM NPF exits, EXITINFO1 describes the fault access and
        // EXITINFO2 carries the faulting guest physical address.
        let is_write = info.exit_info_1.get_bit(1);
        let is_execute = info.exit_info_1.get_bit(4);
        let mut access_flags = MappingFlags::empty();
        if !is_write && !is_execute {
            access_flags |= MappingFlags::READ;
        }
        if is_write {
            access_flags |= MappingFlags::WRITE;
        }
        if is_execute {
            access_flags |= MappingFlags::EXECUTE;
        }
        Ok(NestedPageFaultInfo {
            access_flags,
            fault_guest_paddr: GuestPhysAddr::from(info.exit_info_2 as usize),
        })
    }

    pub fn regs(&self) -> &GeneralRegisters {
        &self.guest_regs
    }

    pub fn regs_mut(&mut self) -> &mut GeneralRegisters {
        &mut self.guest_regs
    }

    pub fn stack_pointer(&self) -> usize {
        unsafe { self.vmcb.as_vmcb_ref().state.rsp.get() as usize }
    }

    pub fn set_stack_pointer(&mut self, rsp: usize) {
        unsafe { self.vmcb.as_vmcb().state.rsp.set(rsp as u64) };
    }

    /// Advance the guest `RIP`; use SVM's decoded next-RIP when available.
    pub fn advance_rip(&mut self, instr_len: u8) -> AxResult {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        let rip = vmcb.state.rip.get();
        let next_rip = vmcb.control.next_rip.get();
        if next_rip > rip {
            vmcb.state.rip.set(next_rip);
        } else {
            vmcb.state.rip.set(rip + instr_len as u64);
        }
        Ok(())
    }

    fn set_rip(&mut self, rip: u64) {
        unsafe { self.vmcb.as_vmcb().state.rip.set(rip) };
    }

    pub fn set_cr(&mut self, cr_idx: usize, val: u64) -> AxResult {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        match cr_idx {
            0 => {
                vmcb.state.cr0.set(val);
                // CR0.PG can activate/deactivate long mode when EFER.LME is set.
                self.sync_long_mode_active();
                self.flush_guest_tlb();
            }
            3 => {
                vmcb.state.cr3.set(val);
                self.flush_guest_tlb();
            }
            4 => {
                vmcb.state.cr4.set(val & !SVM_UNSUPPORTED_GUEST_CR4);
                self.flush_guest_tlb();
            }
            _ => return ax_err!(InvalidInput, format_args!("Unsupported CR{}", cr_idx)),
        }
        Ok(())
    }

    fn handle_local_apic_eoi(&mut self) -> Option<u8> {
        self.vlapic.handle_eoi()
    }

    fn has_pending_external_event(&self, vector: u8) -> bool {
        vector >= 32
            && self
                .pending_events
                .iter()
                .any(|event| event.vector == vector)
    }

    /// Add a virtual interrupt or exception to the pending events list.
    pub fn queue_event(&mut self, vector: u8, err_code: Option<u32>) {
        self.queue_event_with_trigger(vector, err_code, false);
    }

    /// Add a virtual interrupt or exception with trigger mode metadata.
    pub fn queue_event_with_trigger(
        &mut self,
        vector: u8,
        err_code: Option<u32>,
        level_triggered: bool,
    ) {
        if self.has_pending_external_event(vector) {
            return;
        }

        self.pending_events.push_back(PendingEvent {
            vector,
            err_code,
            level_triggered,
        });
    }

    fn flush_guest_tlb(&mut self) {
        unsafe {
            self.vmcb
                .as_vmcb()
                .control
                .tlb_control
                .modify(VmcbTlbControl::CONTROL::FlushGuestTlb);
        }
    }

    fn load_guest_xstate(&mut self) {
        self.xstate.switch_to_guest();
    }

    fn load_host_xstate(&mut self) {
        self.xstate.switch_to_host();
    }

    fn inner_run(&mut self) -> AxResult<super::vmcb::SvmExitInfo> {
        loop {
            self.inject_pending_events()?;
            unsafe {
                self.svm_run();
            }
            self.launched = true;

            self.complete_event_injection();
            self.clear_event_inj();

            let exit_info = self.exit_info()?;

            // Consume exits that are fully handled inside the architecture
            // backend; only unresolved exits are forwarded to the VMM layer.
            if let Some(result) = self.builtin_vmexit_handler(&exit_info) {
                result?;
                continue;
            }

            return Ok(exit_info);
        }
    }

    fn builtin_vmexit_handler(&mut self, exit_info: &super::vmcb::SvmExitInfo) -> Option<AxResult> {
        match exit_info.exit_code {
            Ok(SvmExitCode::CPUID) => Some(self.handle_cpuid()),
            Ok(SvmExitCode::XSETBV) => Some(self.handle_xsetbv()),
            Ok(SvmExitCode::CR_WRITE(cr @ (0 | 3 | 4))) => {
                Some(self.handle_cr_write(cr as usize, exit_info))
            }
            Ok(SvmExitCode::MSR) if self.regs().rcx as u32 == APIC_BASE_MSR => {
                Some(self.handle_apic_base_msr_access(exit_info))
            }
            Ok(SvmExitCode::MSR) if self.regs().rcx as u32 == Msr::IA32_EFER as u32 => {
                Some(self.handle_efer_msr(exit_info))
            }
            Ok(SvmExitCode::MSR)
                if matches!(self.regs().rcx as u32, IA32_UMWAIT_CONTROL | AMD64_DE_CFG) =>
            {
                Some(self.handle_ignored_msr_access(exit_info))
            }
            Ok(SvmExitCode::VINTR) => {
                self.set_interrupt_window(false);
                Some(self.inject_pending_events())
            }
            _ => None,
        }
    }

    fn handle_cr_write(&mut self, cr_idx: usize, exit_info: &super::vmcb::SvmExitInfo) -> AxResult {
        // SVM CR-write exits encode the source GPR in EXITINFO1[3:0].
        let reg_idx = exit_info.exit_info_1.get_bits(0..4) as u8;
        let value = self.gpr_for_cr_access(reg_idx)?;
        self.set_cr(cr_idx, value)?;
        self.advance_rip(3)
    }

    fn gpr_for_cr_access(&self, reg_idx: u8) -> AxResult<u64> {
        // For SVM CR access exits, EXITINFO1[3:0] identifies the source GPR.
        if reg_idx == 4 {
            Ok(unsafe { self.vmcb.as_vmcb_ref().state.rsp.get() })
        } else if reg_idx < 16 {
            Ok(self.regs().get_reg_of_index(reg_idx))
        } else {
            ax_err!(
                InvalidData,
                format_args!("invalid SVM CR access GPR index {reg_idx}")
            )
        }
    }

    fn handle_efer_msr(&mut self, exit_info: &super::vmcb::SvmExitInfo) -> AxResult {
        const VM_EXIT_INSTR_LEN_MSR: u8 = 2;
        let value = self.read_edx_eax();
        if exit_info.exit_info_1 == 0 {
            // EFER.SVME is required by SVM hardware but is not guest-visible.
            let efer = self.guest_visible_efer();
            self.regs_mut().rax = efer & 0xffff_ffff;
            self.regs_mut().rdx = efer >> 32;
        } else {
            self.set_guest_efer(value);
        }
        self.advance_rip(VM_EXIT_INSTR_LEN_MSR)
    }

    fn handle_apic_base_msr_access(&mut self, exit_info: &super::vmcb::SvmExitInfo) -> AxResult {
        const VM_EXIT_INSTR_LEN_MSR: u8 = 2;

        if exit_info.exit_info_1 == 0 {
            self.write_edx_eax(self.vlapic.apic_base());
        } else {
            self.vlapic.set_apic_base(self.read_edx_eax())?;
        }
        self.advance_rip(VM_EXIT_INSTR_LEN_MSR)
    }

    fn handle_apic_msr_access(
        &mut self,
        exit_info: &super::vmcb::SvmExitInfo,
        msr: u32,
    ) -> AxResult<VmExit> {
        const VM_EXIT_INSTR_LEN_MSR: u8 = 2;
        let write = exit_info.exit_info_1 != 0;

        if write {
            if msr == X2APIC_EOI_MSR {
                self.advance_rip(VM_EXIT_INSTR_LEN_MSR)?;
                return Ok(VmExit::InterruptEnd {
                    vector: self.handle_local_apic_eoi(),
                });
            } else {
                let value = self.read_edx_eax() as usize;
                <EmulatedLocalApic as BaseDeviceOps<SysRegAddrRange>>::handle_write(
                    &self.vlapic,
                    SysRegAddr::new(msr as usize),
                    AccessWidth::Qword,
                    value,
                )?;
            }
        } else {
            let value = <EmulatedLocalApic as BaseDeviceOps<SysRegAddrRange>>::handle_read(
                &self.vlapic,
                SysRegAddr::new(msr as usize),
                AccessWidth::Qword,
            )? as u64;
            self.write_edx_eax(value);
        }

        self.advance_rip(VM_EXIT_INSTR_LEN_MSR)?;
        Ok(VmExit::Nothing)
    }

    fn handle_ignored_msr_access(&mut self, exit_info: &super::vmcb::SvmExitInfo) -> AxResult {
        const VM_EXIT_INSTR_LEN_MSR: u8 = 2;

        // Reads return zero, writes are silently discarded. Only call this
        // for known-ignorable MSRs (UMWAIT_CONTROL, AMD64_DE_CFG).
        if exit_info.exit_info_1 == 0 {
            self.write_edx_eax(0);
        }
        self.advance_rip(VM_EXIT_INSTR_LEN_MSR)
    }

    fn guest_visible_efer(&self) -> u64 {
        unsafe { self.vmcb.as_vmcb_ref().state.efer.get() & !EFER_SVME }
    }

    fn set_guest_efer(&mut self, value: u64) {
        unsafe {
            // Preserve SVME in the VMCB even if the guest writes EFER without it.
            self.vmcb.as_vmcb().state.efer.set(value | EFER_SVME);
        }
        self.sync_long_mode_active();
    }

    fn sync_long_mode_active(&mut self) {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        let cr0 = vmcb.state.cr0.get();
        let mut efer = vmcb.state.efer.get() | EFER_SVME;
        if cr0 & CR0_PG != 0 && efer & EFER_LME != 0 {
            efer |= EFER_LMA;
        } else {
            efer &= !EFER_LMA;
        }
        vmcb.state.efer.set(efer);
    }

    fn handle_cpuid(&mut self) -> AxResult {
        use raw_cpuid::{CpuIdResult, cpuid};

        const VM_EXIT_INSTR_LEN_CPUID: u8 = 2;
        const LEAF_FEATURE_INFO: u32 = 0x1;
        const LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION: u32 = 0x7;
        const LEAF_PROCESSOR_EXTENDED_STATE_ENUMERATION: u32 = 0xd;
        const LEAF_EXTENDED_FEATURE_INFO: u32 = 0x8000_0001;
        const LEAF_SVM_FEATURES: u32 = 0x8000_000a;
        const EAX_FREQUENCY_INFO: u32 = 0x16;
        const LEAF_HYPERVISOR_INFO: u32 = 0x4000_0000;
        const LEAF_HYPERVISOR_FEATURE: u32 = 0x4000_0001;
        const VENDOR_STR: &[u8; 12] = b"RVMRVMRVMRVM";
        let vendor_regs = [
            u32::from_le_bytes([VENDOR_STR[0], VENDOR_STR[1], VENDOR_STR[2], VENDOR_STR[3]]),
            u32::from_le_bytes([VENDOR_STR[4], VENDOR_STR[5], VENDOR_STR[6], VENDOR_STR[7]]),
            u32::from_le_bytes([VENDOR_STR[8], VENDOR_STR[9], VENDOR_STR[10], VENDOR_STR[11]]),
        ];

        let regs_clone = *self.regs();
        let function = regs_clone.rax as u32;
        let res = match function {
            LEAF_FEATURE_INFO => {
                const FEATURE_VMX: u32 = 1 << 5;
                const FEATURE_PCID: u32 = 1 << 17;
                const FEATURE_HYPERVISOR: u32 = 1 << 31;
                const FEATURE_MCE: u32 = 1 << 7;
                const FEATURE_X2APIC: u32 = 1 << 21;
                const FEATURE_TSC_DEADLINE: u32 = 1 << 24;
                const FEATURE_APIC: u32 = 1 << 9;
                const MAX_LOGICAL_PROCESSORS_MASK: u32 = 0xff << 16;
                const INITIAL_APIC_ID_MASK: u32 = 0xff << 24;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                // Do not expose nested hardware virtualization to the guest.
                res.ecx &= !FEATURE_VMX;
                res.ecx &= !FEATURE_PCID;
                res.ecx |= FEATURE_X2APIC;
                res.ecx &= !FEATURE_TSC_DEADLINE;
                res.ecx |= FEATURE_HYPERVISOR;
                res.edx &= !FEATURE_MCE;
                res.edx |= FEATURE_APIC;
                res.ebx &= !(MAX_LOGICAL_PROCESSORS_MASK | INITIAL_APIC_ID_MASK);
                res.ebx |= 1 << 16;
                res
            }
            0xb | 0x1f => CpuIdResult {
                eax: 0,
                ebx: 0,
                ecx: regs_clone.rcx as u32,
                edx: 0,
            },
            LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION => {
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if regs_clone.rcx == 0 {
                    // EBX feature flags.
                    const FEATURE_FSGSBASE: u32 = 1 << 0;
                    const FEATURE_SMEP: u32 = 1 << 7;
                    const FEATURE_SMAP: u32 = 1 << 20;
                    // ECX feature flags.
                    const FEATURE_UMIP: u32 = 1 << 2;
                    const FEATURE_PKU: u32 = 1 << 3;
                    const FEATURE_OSPKE: u32 = 1 << 4;
                    const FEATURE_WAITPKG: u32 = 1 << 5;
                    const FEATURE_CET_SS: u32 = 1 << 7;
                    const FEATURE_LA57: u32 = 1 << 16;
                    const FEATURE_PKS: u32 = 1 << 31;
                    // EDX feature flags.
                    const FEATURE_IBT: u32 = 1 << 20;

                    res.ebx &= !(FEATURE_FSGSBASE | FEATURE_SMEP | FEATURE_SMAP);
                    res.ecx &= !(FEATURE_UMIP
                        | FEATURE_PKU
                        | FEATURE_OSPKE
                        | FEATURE_WAITPKG
                        | FEATURE_CET_SS
                        | FEATURE_LA57
                        | FEATURE_PKS);
                    res.edx &= !FEATURE_IBT;
                }
                res
            }
            LEAF_PROCESSOR_EXTENDED_STATE_ENUMERATION => {
                self.load_guest_xstate();
                let res = cpuid!(regs_clone.rax, regs_clone.rcx);
                self.load_host_xstate();
                res
            }
            LEAF_EXTENDED_FEATURE_INFO => {
                const FEATURE_SVM: u32 = 1 << 2;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                // Hide SVM support from the guest until nested SVM is implemented.
                res.ecx &= !FEATURE_SVM;
                res
            }
            LEAF_SVM_FEATURES => CpuIdResult {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            },
            LEAF_HYPERVISOR_INFO => CpuIdResult {
                eax: LEAF_HYPERVISOR_FEATURE,
                ebx: vendor_regs[0],
                ecx: vendor_regs[1],
                edx: vendor_regs[2],
            },
            LEAF_HYPERVISOR_FEATURE => CpuIdResult {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            },
            EAX_FREQUENCY_INFO => {
                const FALLBACK_TSC_FREQUENCY_MHZ: u32 = 3_000;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if res.eax == 0 {
                    let frequency_mhz =
                        crate::host_tsc_frequency_mhz().unwrap_or(FALLBACK_TSC_FREQUENCY_MHZ);
                    warn!(
                        "handle_cpuid: Failed to get TSC frequency by CPUID, default to \
                         {frequency_mhz} MHz"
                    );
                    res.eax = frequency_mhz;
                }
                res
            }
            _ => cpuid!(regs_clone.rax, regs_clone.rcx),
        };

        let regs = self.regs_mut();
        regs.rax = res.eax as _;
        regs.rbx = res.ebx as _;
        regs.rcx = res.ecx as _;
        regs.rdx = res.edx as _;
        self.advance_rip(VM_EXIT_INSTR_LEN_CPUID)
    }

    fn handle_xsetbv(&mut self) -> AxResult {
        const XCR_XCR0: u64 = 0;
        const VM_EXIT_INSTR_LEN_XSETBV: u8 = 3;

        let index = self.guest_regs.rcx.get_bits(0..32);
        let value = self.guest_regs.rdx.get_bits(0..32) << 32 | self.guest_regs.rax.get_bits(0..32);

        if index == XCR_XCR0 {
            Xcr0::from_bits(value)
                .and_then(|x| {
                    if !x.contains(Xcr0::XCR0_FPU_MMX_STATE) {
                        return None;
                    }
                    if x.contains(Xcr0::XCR0_AVX_STATE) && !x.contains(Xcr0::XCR0_SSE_STATE) {
                        return None;
                    }
                    if x.contains(Xcr0::XCR0_BNDCSR_STATE) ^ x.contains(Xcr0::XCR0_BNDREG_STATE) {
                        return None;
                    }

                    let avx512_state = x.contains(Xcr0::XCR0_OPMASK_STATE)
                        || x.contains(Xcr0::XCR0_ZMM_HI256_STATE)
                        || x.contains(Xcr0::XCR0_HI16_ZMM_STATE);
                    let avx512_state_complete = x.contains(Xcr0::XCR0_OPMASK_STATE)
                        && x.contains(Xcr0::XCR0_ZMM_HI256_STATE)
                        && x.contains(Xcr0::XCR0_HI16_ZMM_STATE);
                    if avx512_state
                        && (!avx512_state_complete
                            || !x.contains(Xcr0::XCR0_AVX_STATE)
                            || !x.contains(Xcr0::XCR0_SSE_STATE))
                    {
                        return None;
                    }

                    Some(x)
                })
                .ok_or_else(|| ax_err_type!(InvalidInput))
                .and_then(|x| {
                    self.xstate.guest_xcr0 = x.bits();
                    self.advance_rip(VM_EXIT_INSTR_LEN_XSETBV)
                })
        } else {
            ax_err!(Unsupported, "only xcr0 is supported")
        }
    }

    fn svm_io_exit_info(
        &self,
        exit_info: &super::vmcb::SvmExitInfo,
    ) -> AxResult<(bool, bool, bool, AccessWidth, Port)> {
        let info = exit_info.exit_info_1;
        // SVM packs IO direction, string/repeat attributes, width, and port
        // into EXITINFO1 for IOIO exits.
        let is_in = info.get_bit(0);
        let is_string = info.get_bit(2);
        let is_repeat = info.get_bit(3);
        let width = AccessWidth::try_from(info.get_bits(4..7) as usize)
            .map_err(|_| ax_err_type!(InvalidData, "invalid SVM IOIO access width"))?;
        let port = Port(info.get_bits(16..32) as u16);
        Ok((is_in, is_string, is_repeat, width, port))
    }

    fn read_edx_eax(&self) -> u64 {
        ((self.regs().rdx & 0xffff_ffff) << 32) | (self.regs().rax & 0xffff_ffff)
    }

    fn write_edx_eax(&mut self, val: u64) {
        self.regs_mut().rax = val & 0xffff_ffff;
        self.regs_mut().rdx = val >> 32;
    }

    fn handle_rdtsc(&mut self) -> AxResult {
        const VM_EXIT_INSTR_LEN_RDTSC: u8 = 2;

        let tsc = unsafe { core::arch::x86_64::_rdtsc() };
        let tsc_offset = unsafe { self.vmcb.as_vmcb_ref().control.tsc_offset.get() };
        self.write_edx_eax(tsc.wrapping_add(tsc_offset));
        self.advance_rip(VM_EXIT_INSTR_LEN_RDTSC)
    }

    fn external_interrupt_exit_vector(&self) -> Option<u8> {
        let info = unsafe { self.vmcb.as_vmcb_ref().control.exit_int_info.get() };
        svm_external_interrupt_exit_vector(info)
    }

    fn allow_external_interrupt(&self) -> bool {
        let vmcb = unsafe { self.vmcb.as_vmcb_ref() };
        svm_external_interrupt_allowed(vmcb.state.rflags.get(), vmcb.control.int_state.get())
    }

    fn set_interrupt_window(&mut self, enable: bool) {
        let control = unsafe { &mut self.vmcb.as_vmcb().control };
        set_interrupt_window_control(control, enable);
    }

    fn inject_pending_events(&mut self) -> AxResult {
        if self.injecting_event.is_some() {
            return Ok(());
        }

        let Some(event) = self.pending_events.front().copied() else {
            return Ok(());
        };

        if event.vector >= 32 {
            if self.allow_external_interrupt() {
                self.set_interrupt_window(false);
                inject_external_interrupt_control(
                    unsafe { &mut self.vmcb.as_vmcb().control },
                    event,
                );
                self.injecting_event = Some(event);
                self.pending_events.pop_front();
            } else {
                self.set_interrupt_window(true);
            }
            return Ok(());
        }

        self.inject_event(event.vector, event.err_code)?;
        self.injecting_event = Some(event);
        self.pending_events.pop_front();
        Ok(())
    }

    fn complete_event_injection(&mut self) {
        let Some(injected) = self.injecting_event.take() else {
            return;
        };

        let vmcb = unsafe { self.vmcb.as_vmcb() };
        let exit_int_info = vmcb.control.exit_int_info.get();
        let exit_int_info_err = vmcb.control.exit_int_info_err.get();

        if let Some(interrupted) =
            interrupted_injected_event(exit_int_info, exit_int_info_err, injected)
        {
            self.pending_events.push_front(interrupted);
            vmcb.control.exit_int_info.set(0);
            vmcb.control.exit_int_info_err.set(0);
            vmcb.control.clean_bits.set(0);
            return;
        }

        if injected.vector >= 32 {
            self.vlapic
                .accept_interrupt(injected.vector, injected.level_triggered);
        }
    }

    fn clear_event_inj(&mut self) {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        vmcb.control.event_inj.set(0);
        vmcb.control.event_inj_err.set(0);
        vmcb.control.clean_bits.set(0);
    }

    fn inject_event(&mut self, vector: u8, err_code: Option<u32>) -> AxResult {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        let int_type = if vector < 32 {
            InterruptType::Exception
        } else {
            InterruptType::External
        };
        let mut event = VmcbIntInfo::from(int_type, vector).bits();
        if let Some(err_code) = err_code {
            event |= VmcbIntInfo::ERROR_CODE.bits();
            vmcb.control.event_inj_err.set(err_code);
        } else {
            vmcb.control.event_inj_err.set(0);
        }
        vmcb.control.event_inj.set(event);
        vmcb.control.clean_bits.set(0);
        Ok(())
    }

    fn gla2gva(&self, guest_rip: GuestVirtAddr) -> GuestVirtAddr {
        if self.get_cpu_mode() == VmCpuMode::Mode64 {
            guest_rip
        } else {
            guest_rip + unsafe { self.vmcb.as_vmcb_ref().state.cs.base.get() as usize }
        }
    }

    fn decode_npt_mmio_access(
        &mut self,
        exit_info: &super::vmcb::SvmExitInfo,
        addr: GuestPhysAddr,
        write: bool,
    ) -> AxResult<Option<(VmExit, u8)>> {
        let addr_usize = addr.as_usize();
        let local_apic =
            (X86_LOCAL_APIC_BASE..X86_LOCAL_APIC_BASE + X86_LOCAL_APIC_SIZE).contains(&addr_usize);
        let ioapic = (X86_IOAPIC_BASE..X86_IOAPIC_BASE + X86_IOAPIC_SIZE).contains(&addr_usize);
        if !local_apic && !ioapic {
            return Ok(None);
        }

        let start = self.gla2gva(GuestVirtAddr::from(exit_info.guest_rip as usize));
        let mut rip = start;
        let mut rex = 0u8;
        if let Err(err) = self.skip_simple_prefixes(&mut rip, &mut rex) {
            debug!("failed to decode SVM NPF MMIO prefixes: {err:?}");
            return Ok(None);
        }

        let opcode = self.read_guest_u8(rip)?;
        rip += 1;
        let modrm = self.read_guest_u8(rip)?;
        rip += 1;
        if modrm >> 6 == 0b11 {
            debug!("SVM NPF MMIO access did not use a memory operand");
            return Ok(None);
        }

        match (write, opcode) {
            (_, opcode) if svm_mmio_register_write_opcode(write, opcode, local_apic) => {
                let reg = ((modrm >> 3) & 0x7) | ((rex & 0x4) << 1);
                let end = self.skip_modrm_memory_operand(rip, modrm, rex)?;
                let data = self.guest_regs.get_reg_of_index(reg) as u32 as u64;
                let exit = self.handle_decoded_npt_mmio_write(addr, data, local_apic)?;
                Ok(Some((exit, (end.as_usize() - start.as_usize()) as u8)))
            }
            (true, 0xc7) if (modrm >> 3) & 0x7 == 0 => {
                let imm_addr = self.skip_modrm_memory_operand(rip, modrm, rex)?;
                let mut data = 0u32;
                for i in 0..size_of::<u32>() {
                    data |= (self.read_guest_u8(imm_addr + i)? as u32) << (i * 8);
                }
                let exit = self.handle_decoded_npt_mmio_write(addr, data as u64, local_apic)?;
                Ok(Some((
                    exit,
                    (imm_addr.as_usize() + size_of::<u32>() - start.as_usize()) as u8,
                )))
            }
            (false, 0x8b) => {
                let reg = (((modrm >> 3) & 0x7) | ((rex & 0x4) << 1)) as usize;
                let end = self.skip_modrm_memory_operand(rip, modrm, rex)?;
                let exit = if local_apic {
                    let val =
                        <EmulatedLocalApic as BaseDeviceOps<AddrRange<GuestPhysAddr>>>::handle_read(
                            &self.vlapic,
                            addr,
                            AccessWidth::Dword,
                        )?;
                    self.regs_mut()
                        .set_reg_of_index(reg as u8, val as u32 as u64);
                    VmExit::Nothing
                } else {
                    VmExit::MmioRead {
                        addr,
                        width: AccessWidth::Dword,
                        reg,
                        reg_width: AccessWidth::Dword,
                        signed_ext: false,
                    }
                };
                Ok(Some((exit, (end.as_usize() - start.as_usize()) as u8)))
            }
            _ => {
                debug!("unsupported SVM NPF MMIO opcode {opcode:#x}, write={write}");
                Ok(None)
            }
        }
    }

    fn handle_decoded_npt_mmio_write(
        &mut self,
        addr: GuestPhysAddr,
        data: u64,
        local_apic: bool,
    ) -> AxResult<VmExit> {
        if !local_apic {
            return Ok(VmExit::MmioWrite {
                addr,
                width: AccessWidth::Dword,
                data,
            });
        }

        let offset = addr.as_usize() - X86_LOCAL_APIC_BASE;
        if offset == X86_LOCAL_APIC_EOI_OFFSET {
            return Ok(VmExit::InterruptEnd {
                vector: self.handle_local_apic_eoi(),
            });
        }

        <EmulatedLocalApic as BaseDeviceOps<AddrRange<GuestPhysAddr>>>::handle_write(
            &self.vlapic,
            addr,
            AccessWidth::Dword,
            data as usize,
        )?;
        Ok(VmExit::Nothing)
    }

    fn skip_simple_prefixes(&self, rip: &mut GuestVirtAddr, rex: &mut u8) -> AxResult {
        loop {
            let byte = self.read_guest_u8(*rip)?;
            if byte == 0x66 {
                *rip += 1;
            } else if (0x40..=0x4f).contains(&byte) {
                *rex = byte;
                *rip += 1;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_modrm_memory_operand(
        &self,
        mut cursor: GuestVirtAddr,
        modrm: u8,
        rex: u8,
    ) -> AxResult<GuestVirtAddr> {
        let mode = modrm >> 6;
        let rm = modrm & 0x7;

        if rm == 0b100 {
            let sib = self.read_guest_u8(cursor)?;
            cursor += 1;
            let base = sib & 0x7;
            if mode == 0 && base == 0b101 {
                cursor += size_of::<u32>();
            }
        } else if mode == 0 && rm == 0b101 && rex & 0x1 == 0 {
            cursor += size_of::<u32>();
        }

        match mode {
            0 => {}
            1 => cursor += size_of::<u8>(),
            2 => cursor += size_of::<u32>(),
            _ => return ax_err!(InvalidInput, "ModRM register operand is not memory"),
        }

        Ok(cursor)
    }

    fn read_guest_u8(&self, gva: GuestVirtAddr) -> AxResult<u8> {
        let gpa = self.translate_guest_linear(gva)?;
        let hva = crate::host::phys_to_virt(HostPhysAddr::from(gpa.as_usize()));
        Ok(unsafe { core::ptr::read_volatile(hva.as_ptr()) })
    }

    fn translate_guest_linear(&self, gva: GuestVirtAddr) -> AxResult<GuestPhysAddr> {
        let addr = gva.as_usize();
        match self.get_paging_level() {
            0 => Ok(GuestPhysAddr::from(addr)),
            4 => self.walk_guest_page_table_4level(addr),
            level => ax_err!(
                Unsupported,
                format_args!("unsupported SVM MMIO decode paging level {level}")
            ),
        }
    }

    fn get_paging_level(&self) -> usize {
        let vmcb = unsafe { self.vmcb.as_vmcb_ref() };
        let mut level = 0;
        let cr0 = vmcb.state.cr0.get();
        let cr4 = vmcb.state.cr4.get();
        let efer = vmcb.state.efer.get();
        if cr0 & Cr0Flags::PAGING.bits() != 0 {
            if cr4 & Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits() != 0 {
                if efer & EferFlags::LONG_MODE_ACTIVE.bits() != 0 {
                    level = 4;
                } else {
                    level = 3;
                }
            } else {
                level = 2;
            }
        }
        level
    }

    fn walk_guest_page_table_4level(&self, gva: usize) -> AxResult<GuestPhysAddr> {
        const PRESENT: u64 = 1 << 0;
        const HUGE_PAGE: u64 = 1 << 7;
        const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
        const PAGE_4K_MASK: usize = 0xfff;
        const PAGE_2M_MASK: usize = 0x1f_ffff;
        const PAGE_1G_MASK: usize = 0x3fff_ffff;

        let mut table = unsafe { self.vmcb.as_vmcb_ref().state.cr3.get() } & ADDR_MASK;
        let indexes = [
            (gva >> 39) & 0x1ff,
            (gva >> 30) & 0x1ff,
            (gva >> 21) & 0x1ff,
            (gva >> 12) & 0x1ff,
        ];

        for (level, index) in indexes.into_iter().enumerate() {
            let entry = read_guest_phys_u64(table as usize + index * size_of::<u64>());
            if entry & PRESENT == 0 {
                return ax_err!(
                    InvalidInput,
                    format_args!("guest RIP page table entry is not present at level {level}")
                );
            }

            let paddr = (entry & ADDR_MASK) as usize;
            match level {
                1 if entry & HUGE_PAGE != 0 => {
                    return Ok(GuestPhysAddr::from(paddr + (gva & PAGE_1G_MASK)));
                }
                2 if entry & HUGE_PAGE != 0 => {
                    return Ok(GuestPhysAddr::from(paddr + (gva & PAGE_2M_MASK)));
                }
                3 => return Ok(GuestPhysAddr::from(paddr + (gva & PAGE_4K_MASK))),
                _ => table = paddr as u64,
            }
        }

        ax_err!(InvalidInput, "failed to translate guest RIP")
    }

    fn before_vmrun(&mut self) {
        let rax = self.regs().rax;
        unsafe {
            super::instructions::clgi();
            self.vmcb.as_vmcb().state.rax.set(rax);
        }
        self.load_save_states.save();
        unsafe {
            let _ = super::instructions::vmload(self.vmcb.phys_addr().as_usize() as u64);
        }
    }

    fn after_vmrun(&mut self) {
        unsafe {
            let _ = super::instructions::vmsave(self.vmcb.phys_addr().as_usize() as u64);
        }
        self.load_save_states.load();
        self.regs_mut().rax = unsafe { self.vmcb.as_vmcb().state.rax.get() };
        unsafe {
            super::instructions::stgi();
        }
    }

    /// Enter the guest once with AMD SVM `VMRUN`.
    ///
    /// # Safety
    ///
    /// The caller must ensure SVM is enabled on the current CPU, the VMCB and
    /// nested page table referenced by this vCPU are valid, and host state is
    /// restored after the VM exit before returning to normal Rust execution.
    pub unsafe fn svm_run(&mut self) {
        let self_addr = self as *mut Self as u64;
        let vmcb = self.vmcb.phys_addr().as_usize() as u64;

        self.load_guest_xstate();
        self.before_vmrun();

        // Keep the register save/restore sequence adjacent to VMRUN; Rust calls
        // in this window may clobber the guest registers prepared for entry.
        // SVM samples host RFLAGS.IF at VMRUN, while GIF remains closed until
        // after the host state has been restored from the exit path.
        unsafe {
            asm!(
                "pushfq", // save host RFLAGS, including IF
                "pop qword ptr [rdi + {host_rflags}]",
                "sti",
                save_regs_no_rax!(),
                "mov [rdi + {host_stack_top}], rsp",
                "mov rsp, rdi",
                restore_regs_no_rax!(),
                "vmrun rax",
                "cli", // keep host IRQs off until host xstate is restored
                save_regs_no_rax!(),
                "mov rdi, rsp",
                "mov rsp, [rdi + {host_stack_top}]",
                restore_regs_no_rax!(),
                host_stack_top = const size_of::<GeneralRegisters>(),
                host_rflags = const size_of::<GeneralRegisters>() + size_of::<u64>(),
                in("rax") vmcb,
                in("rdi") self_addr,
            );
        }

        self.after_vmrun();
        self.load_host_xstate();
        restore_host_interrupt_flag(self.host_rflags);
    }
}

fn inject_external_interrupt_control(
    control: &mut super::vmcb::VmcbControlArea,
    event: PendingEvent,
) {
    control.event_inj.set(
        VmcbIntInfo::from(InterruptType::External, event.vector).bits()
            & !VmcbIntInfo::ERROR_CODE.bits(),
    );
    control.event_inj_err.set(0);
    control.clean_bits.set(0);
}

fn svm_external_interrupt_allowed(rflags: u64, int_state: u32) -> bool {
    rflags & RFlags::INTERRUPT_FLAG.bits() != 0 && int_state & SVM_INT_STATE_INTERRUPT_SHADOW == 0
}

fn svm_external_interrupt_exit_vector(info: u32) -> Option<u8> {
    let int_info = VmcbIntInfo::from_bits_retain(info);
    let int_type = (info >> 8) & 0b111;
    (int_info.contains(VmcbIntInfo::VALID) && int_type == InterruptType::External as u32)
        .then_some((info & 0xff) as u8)
}

fn interrupted_injected_event(info: u32, err: u32, injected: PendingEvent) -> Option<PendingEvent> {
    let int_info = VmcbIntInfo::from_bits_retain(info);
    if !int_info.contains(VmcbIntInfo::VALID) {
        return None;
    }

    let vector = (info & 0xff) as u8;
    let int_type = (info >> 8) & 0b111;
    if vector != injected.vector || int_type != pending_event_interrupt_type(injected) {
        return None;
    }

    let err_code = if int_type == InterruptType::Exception as u32 {
        if int_info.contains(VmcbIntInfo::ERROR_CODE) {
            Some(err)
        } else {
            injected.err_code
        }
    } else {
        None
    };

    Some(PendingEvent {
        vector,
        err_code,
        level_triggered: injected.level_triggered,
    })
}

fn pending_event_interrupt_type(event: PendingEvent) -> u32 {
    if event.vector < 32 {
        InterruptType::Exception as u32
    } else {
        InterruptType::External as u32
    }
}

fn svm_intr_exit_reason(_vector: Option<u8>) -> VmExit {
    // SVM_EXIT_INTR is a host IRQ exit point. Unlike VMX external-interrupt
    // exits, VMCB exit_int_info is not a reliable dispatch key for the host
    // IRQ framework, so the caller must let the host consume the pending IRQ.
    VmExit::PreemptionTimer
}

fn service_pending_host_interrupt() {
    let host_rflags = current_rflags();
    unsafe {
        // SVM's INTR exit does not always provide the host vector. Briefly
        // open host interrupts so the pending physical IRQ is consumed by the
        // platform IRQ framework, then restore the previous host IF state.
        asm!("sti", "nop", options(nomem, nostack));
    }
    restore_host_interrupt_flag(host_rflags);
}

fn current_rflags() -> u64 {
    let flags: u64;
    unsafe {
        asm!(
            "pushfq",
            "pop {flags}",
            flags = lateout(reg) flags,
            options(nomem, preserves_flags),
        );
    }
    flags
}

fn svm_mmio_register_write_opcode(write: bool, opcode: u8, local_apic: bool) -> bool {
    // Linux xAPIC writes use alternative_io(): affected CPUs may patch the
    // usual movl into xchgl while keeping the same MMIO write side effect.
    // SVM can report the read phase of xchg first, so keep the side effect.
    matches!((write, opcode), (true, 0x89)) || (local_apic && opcode == 0x87)
}

fn set_interrupt_window_control(control: &mut super::vmcb::VmcbControlArea, enable: bool) {
    if enable {
        let priority = 0xf << SVM_INT_CTL_V_INTR_PRIO_SHIFT;
        let int_control = (control.int_control.get() & !SVM_INT_CTL_V_INTR_PRIO_MASK)
            | SVM_INT_CTL_V_IRQ
            | priority
            | SVM_INT_CTL_V_INTR_MASKING;
        control.int_vector.set(0);
        control.int_control.set(int_control);
        control.set_intercept(SvmIntercept::VINTR);
    } else {
        control
            .int_control
            .set(control.int_control.get() & !SVM_INT_CTL_V_IRQ_INJECTION_BITS);
        control
            .intercept_vector3
            .modify(super::vmcb::InterceptVec3::VINTR::CLEAR);
    }
    control.clean_bits.set(0);
}

fn enable_virtual_interrupt_masking_control(control: &mut super::vmcb::VmcbControlArea) {
    control
        .int_control
        .set(control.int_control.get() | SVM_INT_CTL_V_INTR_MASKING);
    control.clean_bits.set(0);
}

impl Debug for SvmVcpu {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("SvmVcpu")
            .field("entry", &self.entry)
            .field("npt_root", &self.npt_root)
            .field("vmcb", &self.vmcb.phys_addr())
            .field("launched", &self.launched)
            .finish()
    }
}

impl VmArchVcpuOps for SvmVcpu {
    type CreateConfig = X86VCpuCreateConfig;
    type SetupConfig = X86VCpuSetupConfig;

    fn new(vm_id: VMId, vcpu_id: VCpuId, _config: Self::CreateConfig) -> AxResult<Self> {
        Self::create(vm_id, vcpu_id)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.entry = Some(entry);
        Ok(())
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxResult {
        self.npt_root = Some(config.root_paddr);
        Ok(())
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        let entry = self
            .entry
            .ok_or_else(|| ax_err_type!(InvalidInput, "SVM guest entry is not set"))?;
        let npt_root = self
            .npt_root
            .ok_or_else(|| ax_err_type!(InvalidInput, "SVM NPT root is not set"))?;
        self.setup_vmcb(entry, npt_root, config)
    }

    fn run(&mut self) -> AxResult<VmExit> {
        {
            let exit_info = self.inner_run()?;
            let exit_code = match exit_info.exit_code {
                Ok(code) => code,
                Err(code) => {
                    warn!("SVM unknown VM-exit code: {code:#x}, exit_info: {exit_info:#x?}");
                    return Ok(VmExit::Halt);
                }
            };

            Ok(match exit_code {
                SvmExitCode::INVALID | SvmExitCode::BUSY => VmExit::FailEntry {
                    hardware_entry_failure_reason: match exit_code {
                        SvmExitCode::INVALID => u64::MAX,
                        SvmExitCode::BUSY => u64::MAX - 1,
                        _ => unreachable!(),
                    },
                },
                SvmExitCode::VMMCALL => {
                    self.advance_rip(3)?;
                    VmExit::Hypercall {
                        nr: self.regs().rax,
                        args: [
                            self.regs().rdi,
                            self.regs().rsi,
                            self.regs().rdx,
                            self.regs().rcx,
                            self.regs().r8,
                            self.regs().r9,
                        ],
                    }
                }
                SvmExitCode::RDTSC => {
                    self.handle_rdtsc()?;
                    VmExit::PreemptionTimer
                }
                SvmExitCode::IOIO => {
                    let (is_in, is_string, is_repeat, width, port) =
                        self.svm_io_exit_info(&exit_info)?;
                    // IOIO exits provide the decoded next RIP in EXITINFO2.
                    self.set_rip(exit_info.exit_info_2);

                    if is_string || is_repeat {
                        warn!("SVM unsupported IOIO exit: {exit_info:#x?}");
                        warn!("VCpu {self:#x?}");
                        VmExit::Halt
                    } else if is_in {
                        VmExit::IoRead { port, width }
                    } else if port == Port(QEMU_EXIT_PORT)
                        && width == AccessWidth::Word
                        && self.regs().rax == QEMU_EXIT_MAGIC
                    {
                        VmExit::SystemDown
                    } else {
                        VmExit::IoWrite {
                            port,
                            width,
                            data: self.regs().rax.get_bits(width.bits_range()),
                        }
                    }
                }
                SvmExitCode::MSR => {
                    let msr = self.regs().rcx as u32;
                    if (X2APIC_MSR_BASE..=X2APIC_MSR_END).contains(&msr) {
                        self.handle_apic_msr_access(&exit_info, msr)?
                    } else {
                        self.advance_rip(2)?;
                        if exit_info.exit_info_1 == 0 {
                            VmExit::SysRegRead {
                                addr: SysRegAddr::new(self.regs().rcx as _),
                                reg: 0,
                            }
                        } else {
                            VmExit::SysRegWrite {
                                addr: SysRegAddr::new(self.regs().rcx as _),
                                value: self.read_edx_eax(),
                            }
                        }
                    }
                }
                SvmExitCode::NPF => {
                    let info = self.nested_page_fault_info()?;
                    let write = info.access_flags.contains(MappingFlags::WRITE);
                    let read = info.access_flags.contains(MappingFlags::READ);
                    if (read || write)
                        && let Some((mmio_exit, instr_len)) =
                            self.decode_npt_mmio_access(&exit_info, info.fault_guest_paddr, write)?
                    {
                        self.advance_rip(instr_len)?;
                        mmio_exit
                    } else {
                        VmExit::NestedPageFault {
                            addr: info.fault_guest_paddr,
                            access_flags: info.access_flags,
                        }
                    }
                }
                SvmExitCode::INTR => {
                    // SVM has no VMX-style preemption timer. Use INTR exits
                    // as a periodic VMM poll point after first letting the
                    // host consume the pending physical IRQ.
                    let vector = self.external_interrupt_exit_vector();
                    service_pending_host_interrupt();
                    svm_intr_exit_reason(vector)
                }
                SvmExitCode::HLT => {
                    self.advance_rip(1)?;
                    VmExit::PreemptionTimer
                }
                SvmExitCode::PAUSE => {
                    self.advance_rip(2)?;
                    VmExit::PreemptionTimer
                }
                SvmExitCode::SHUTDOWN => VmExit::SystemDown,
                _ => {
                    warn!("SVM unsupported VM-exit: {exit_info:#x?}");
                    warn!("VCpu {self:#x?}");
                    VmExit::Halt
                }
            })
        }
    }

    fn bind(&mut self) -> AxResult {
        self.bind_to_current_processor()
    }

    fn unbind(&mut self) -> AxResult {
        self.unbind_from_current_processor()
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.regs_mut().set_reg_of_index(reg as u8, val as u64);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        if vector == 0 {
            warn!("interrupt queued in inject_interrupt: vector 0");
            panic!()
        }
        self.queue_event(vector as u8, None);
        Ok(())
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: axvm_types::InterruptTriggerMode,
    ) -> AxResult {
        if vector == 0 {
            warn!("interrupt queued in inject_interrupt_with_trigger: vector 0");
            panic!()
        }
        self.queue_event_with_trigger(
            vector as u8,
            None,
            trigger == axvm_types::InterruptTriggerMode::LevelTriggered,
        );
        Ok(())
    }

    fn handle_eoi(&mut self) -> Option<u8> {
        self.handle_local_apic_eoi()
    }

    fn set_return_value(&mut self, val: usize) {
        self.regs_mut().rax = val as u64;
    }
}

#[cfg(test)]
mod tests {
    use core::mem::MaybeUninit;

    use axvm_types::VmExit;
    use tock_registers::interfaces::{Readable, Writeable};
    use x86_64::registers::rflags::RFlags;

    use super::{
        PendingEvent, SVM_INT_CTL_V_INTR_MASKING, SVM_INT_CTL_V_INTR_PRIO_SHIFT, SVM_INT_CTL_V_IRQ,
        SVM_INT_CTL_V_IRQ_INJECTION_BITS, SVM_INT_STATE_INTERRUPT_SHADOW,
        enable_virtual_interrupt_masking_control, inject_external_interrupt_control,
        interrupted_injected_event, set_interrupt_window_control, svm_external_interrupt_allowed,
        svm_external_interrupt_exit_vector, svm_intr_exit_reason, svm_mmio_register_write_opcode,
    };
    use crate::svm::{
        flags::{InterruptType, VmcbIntInfo},
        vmcb::VmcbControlArea,
    };

    #[test]
    fn svm_external_irq_injection_uses_eventinj_not_v_irq_latch() {
        let mut control = unsafe { MaybeUninit::<VmcbControlArea>::zeroed().assume_init() };
        control.event_inj.set(0);
        control.int_control.set(SVM_INT_CTL_V_INTR_MASKING);

        inject_external_interrupt_control(
            &mut control,
            PendingEvent {
                vector: 0x51,
                err_code: None,
                level_triggered: true,
            },
        );

        let event = control.event_inj.get();
        assert_ne!(event & (1 << 31), 0);
        assert_eq!(event & 0xff, 0x51);
        assert_eq!(event & (1 << 11), 0);
        assert_eq!(control.int_control.get(), SVM_INT_CTL_V_INTR_MASKING);
    }

    #[test]
    fn svm_control_enables_virtual_interrupt_masking() {
        let mut control = unsafe { MaybeUninit::<VmcbControlArea>::zeroed().assume_init() };
        control.int_control.set(0);

        enable_virtual_interrupt_masking_control(&mut control);

        assert_eq!(
            control.int_control.get() & SVM_INT_CTL_V_INTR_MASKING,
            SVM_INT_CTL_V_INTR_MASKING
        );
    }

    #[test]
    fn svm_external_irq_waits_for_guest_interrupt_window() {
        let if_enabled = RFlags::INTERRUPT_FLAG.bits();

        assert!(svm_external_interrupt_allowed(if_enabled, 0));
        assert!(!svm_external_interrupt_allowed(0, 0));
        assert!(!svm_external_interrupt_allowed(
            if_enabled,
            SVM_INT_STATE_INTERRUPT_SHADOW
        ));
    }

    #[test]
    fn svm_interrupt_window_uses_dummy_vintr_and_is_clearable() {
        let mut control = unsafe { MaybeUninit::<VmcbControlArea>::zeroed().assume_init() };
        control.int_control.set(SVM_INT_CTL_V_INTR_MASKING);

        set_interrupt_window_control(&mut control, true);

        assert_ne!(control.int_control.get() & SVM_INT_CTL_V_IRQ, 0);
        assert_eq!(
            control.int_control.get() & (0xf << SVM_INT_CTL_V_INTR_PRIO_SHIFT),
            0xf << SVM_INT_CTL_V_INTR_PRIO_SHIFT
        );
        assert_eq!(control.int_vector.get(), 0);
        assert_ne!(control.intercept_vector3.get() & (1 << 4), 0);
        assert_eq!(control.event_inj.get(), 0);

        set_interrupt_window_control(&mut control, false);

        assert_eq!(
            control.int_control.get() & SVM_INT_CTL_V_IRQ_INJECTION_BITS,
            0
        );
        assert_eq!(
            control.int_control.get() & SVM_INT_CTL_V_INTR_MASKING,
            SVM_INT_CTL_V_INTR_MASKING
        );
        assert_eq!(control.intercept_vector3.get() & (1 << 4), 0);
    }

    #[test]
    fn svm_intr_exit_reports_external_interrupt_vector() {
        let vector = 0x51;
        let info = VmcbIntInfo::from(InterruptType::External, vector).bits();

        assert_eq!(svm_external_interrupt_exit_vector(info), Some(vector));
        assert_eq!(svm_external_interrupt_exit_vector(0), None);
        assert_eq!(
            svm_external_interrupt_exit_vector(
                VmcbIntInfo::from(InterruptType::Exception, 6).bits()
            ),
            None
        );
    }

    #[test]
    fn svm_intr_exit_is_host_poll_point_even_with_exit_int_info() {
        let vector = 0x51;
        let exit = svm_intr_exit_reason(Some(vector));

        assert!(matches!(exit, VmExit::PreemptionTimer));
    }

    #[test]
    fn svm_requeues_interrupted_event_injection() {
        let injected = PendingEvent {
            vector: 0x51,
            err_code: None,
            level_triggered: true,
        };
        let info = VmcbIntInfo::from(InterruptType::External, injected.vector).bits();

        assert_eq!(
            interrupted_injected_event(info, 0, injected),
            Some(injected)
        );
    }

    #[test]
    fn svm_ignores_unrelated_exit_int_info_for_event_completion() {
        let injected = PendingEvent {
            vector: 0x51,
            err_code: None,
            level_triggered: true,
        };
        let unrelated = VmcbIntInfo::from(InterruptType::External, 0x52).bits();

        assert_eq!(interrupted_injected_event(0, 0, injected), None);
        assert_eq!(interrupted_injected_event(unrelated, 0, injected), None);
    }

    #[test]
    fn svm_mmio_decoder_accepts_linux_xapic_xchg_write() {
        assert!(svm_mmio_register_write_opcode(true, 0x89, false));
        assert!(svm_mmio_register_write_opcode(true, 0x89, true));
        assert!(svm_mmio_register_write_opcode(true, 0x87, true));
        assert!(svm_mmio_register_write_opcode(false, 0x87, true));
        assert!(!svm_mmio_register_write_opcode(true, 0x87, false));
        assert!(!svm_mmio_register_write_opcode(false, 0x87, false));
        assert!(!svm_mmio_register_write_opcode(false, 0x8b, true));
    }
}

fn read_guest_phys_u64(gpa: usize) -> u64 {
    let hva = crate::host::phys_to_virt(HostPhysAddr::from(gpa));
    unsafe { core::ptr::read_unaligned(hva.as_ptr() as *const u64) }
}
