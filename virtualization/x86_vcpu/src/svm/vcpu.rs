use core::{
    arch::asm,
    fmt::{Debug, Formatter, Result as FmtResult},
    mem::size_of,
};

use ax_errno::{AxResult, ax_err, ax_err_type};
use axaddrspace::{
    GuestPhysAddr, HostPhysAddr, MappingFlags, NestedPageFaultInfo,
    device::{AccessWidth, Port, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::BaseDeviceOps;
use axvcpu::{AxArchVCpu, AxVCpuExitReason};
use axvisor_api::vmm::{VCpuId, VMId};
use bit_field::BitField;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use x86_64::registers::control::Cr0Flags;
use x86_vlapic::EmulatedLocalApic;

use super::{
    definitions::{SvmExitCode, SvmIntercept},
    structs::{IOPm, MSRPm, VmcbFrame},
    vmcb::{InterceptCrRw, InterceptExceptions, NestedCtl, VmcbTlbControl, set_vmcb_segment},
};
use crate::{
    X86VCpuSetupConfig, msr::Msr, regs::GeneralRegisters, restore_host_interrupt_flag,
    x86_real_mode_entry_state, xstate::XState,
};

const QEMU_EXIT_PORT: u16 = 0x604;
const QEMU_EXIT_MAGIC: u64 = 0x2000;
const QEMU_RESET_PORT: u16 = 0xcf9;

const EFER_SVME: u64 = 1 << 12;
const EFER_LMA: u64 = 1 << 10;
const EFER_LME: u64 = 1 << 8;
const CR0_PG: u64 = 1 << 31;
const CR0_PE: u64 = 1 << 0;
const X2APIC_MSR_BASE: u32 = 0x800;
// Match the current VMX/vLAPIC path, which handles x2APIC register offsets 0x00..=0x3f.
const X2APIC_MSR_END: u32 = 0x83f;

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
            vlapic: EmulatedLocalApic::new(vm_id, vcpu_id),
            xstate: XState::new(),
        };
        info!("[HV] created SvmVcpu(vmcb: {:#x})", vcpu.vmcb.phys_addr());
        Ok(vcpu)
    }

    fn setup_vmcb(&mut self, entry: GuestPhysAddr, npt_root: HostPhysAddr) -> AxResult {
        self.setup_io_bitmap()?;
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
        control.clean_bits.set(0);
        control
            .tlb_control
            .modify(VmcbTlbControl::CONTROL::FlushGuestTlb);
        control.int_control.set(1 << 24);
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
            SvmIntercept::CPUID,
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
        // Keep EFER under software control so the guest never observes or
        // clears the host-required SVME bit stored in the VMCB.
        self.msrpm.set_read_intercept(Msr::IA32_EFER as u32, true);
        self.msrpm.set_write_intercept(Msr::IA32_EFER as u32, true);
        // Route x2APIC MSRs through the emulated local APIC instead of the host APIC.
        for msr in X2APIC_MSR_BASE..=X2APIC_MSR_END {
            self.msrpm.set_read_intercept(msr, true);
            self.msrpm.set_write_intercept(msr, true);
        }
        Ok(())
    }

    fn setup_io_bitmap(&mut self) -> AxResult {
        // These ports are part of the x86 QEMU test contract: 0x604 reports
        // test completion and 0xcf9 requests reset/poweroff.
        self.iopm
            .set_intercept_of_range(QEMU_EXIT_PORT as _, 2, true);
        self.iopm
            .set_intercept_of_range(QEMU_RESET_PORT as _, 1, true);
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
                vmcb.state.cr4.set(val);
                self.flush_guest_tlb();
            }
            _ => return ax_err!(InvalidInput, format_args!("Unsupported CR{}", cr_idx)),
        }
        Ok(())
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
            unsafe {
                self.svm_run();
            }
            self.launched = true;

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
            Ok(SvmExitCode::CR_WRITE(cr @ (0 | 3 | 4))) => {
                Some(self.handle_cr_write(cr as usize, exit_info))
            }
            Ok(SvmExitCode::MSR) if self.regs().rcx as u32 == Msr::IA32_EFER as u32 => {
                Some(self.handle_efer_msr(exit_info))
            }
            Ok(SvmExitCode::MSR)
                if (X2APIC_MSR_BASE..=X2APIC_MSR_END).contains(&(self.regs().rcx as u32)) =>
            {
                Some(self.handle_apic_msr_access(exit_info, self.regs().rcx as u32))
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

    fn handle_apic_msr_access(
        &mut self,
        exit_info: &super::vmcb::SvmExitInfo,
        msr: u32,
    ) -> AxResult {
        const VM_EXIT_INSTR_LEN_MSR: u8 = 2;
        let write = exit_info.exit_info_1 != 0;

        if write {
            let value = self.read_edx_eax() as usize;
            <EmulatedLocalApic as BaseDeviceOps<SysRegAddrRange>>::handle_write(
                &self.vlapic,
                SysRegAddr::new(msr as usize),
                AccessWidth::Qword,
                value,
            )?;
        } else {
            let value = <EmulatedLocalApic as BaseDeviceOps<SysRegAddrRange>>::handle_read(
                &self.vlapic,
                SysRegAddr::new(msr as usize),
                AccessWidth::Qword,
            )? as u64;
            self.write_edx_eax(value);
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
                const FEATURE_HYPERVISOR: u32 = 1 << 31;
                const FEATURE_MCE: u32 = 1 << 7;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                // Do not expose nested hardware virtualization to the guest.
                res.ecx &= !FEATURE_VMX;
                res.ecx |= FEATURE_HYPERVISOR;
                res.edx &= !FEATURE_MCE;
                res
            }
            LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION => {
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if regs_clone.rcx == 0 {
                    res.ecx.set_bit(5, false);
                    res.ecx.set_bit(16, false);
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
        unsafe {
            asm!(
                "pushfq", // save host RFLAGS, including IF
                "pop qword ptr [rdi + {host_rflags}]",
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

impl AxArchVCpu for SvmVcpu {
    type CreateConfig = ();
    type SetupConfig = X86VCpuSetupConfig;

    fn new(vm_id: VMId, vcpu_id: VCpuId, _config: Self::CreateConfig) -> AxResult<Self> {
        Self::create(vm_id, vcpu_id)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.entry = Some(entry);
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.npt_root = Some(ept_root);
        Ok(())
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        let entry = self
            .entry
            .ok_or_else(|| ax_err_type!(InvalidInput, "SVM guest entry is not set"))?;
        let npt_root = self
            .npt_root
            .ok_or_else(|| ax_err_type!(InvalidInput, "SVM NPT root is not set"))?;
        self.setup_vmcb(entry, npt_root)
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        {
            let exit_info = self.inner_run()?;
            let exit_code = match exit_info.exit_code {
                Ok(code) => code,
                Err(code) => {
                    warn!("SVM unknown VM-exit code: {code:#x}, exit_info: {exit_info:#x?}");
                    return Ok(AxVCpuExitReason::Halt);
                }
            };

            Ok(match exit_code {
                SvmExitCode::INVALID | SvmExitCode::BUSY => AxVCpuExitReason::FailEntry {
                    hardware_entry_failure_reason: match exit_code {
                        SvmExitCode::INVALID => u64::MAX,
                        SvmExitCode::BUSY => u64::MAX - 1,
                        _ => unreachable!(),
                    },
                },
                SvmExitCode::VMMCALL => {
                    self.advance_rip(3)?;
                    AxVCpuExitReason::Hypercall {
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
                SvmExitCode::IOIO => {
                    let (is_in, is_string, is_repeat, width, port) =
                        self.svm_io_exit_info(&exit_info)?;
                    // IOIO exits provide the decoded next RIP in EXITINFO2.
                    self.set_rip(exit_info.exit_info_2);

                    if is_string || is_repeat {
                        warn!("SVM unsupported IOIO exit: {exit_info:#x?}");
                        warn!("VCpu {self:#x?}");
                        AxVCpuExitReason::Halt
                    } else if is_in {
                        AxVCpuExitReason::IoRead { port, width }
                    } else if port == Port(QEMU_EXIT_PORT)
                        && width == AccessWidth::Word
                        && self.regs().rax == QEMU_EXIT_MAGIC
                    {
                        AxVCpuExitReason::SystemDown
                    } else if port == Port(QEMU_RESET_PORT) {
                        warn!(
                            "SVM guest wrote QEMU reset port {port:#x} with data {:#x}",
                            self.regs().rax.get_bits(width.bits_range())
                        );
                        AxVCpuExitReason::SystemDown
                    } else {
                        AxVCpuExitReason::IoWrite {
                            port,
                            width,
                            data: self.regs().rax.get_bits(width.bits_range()),
                        }
                    }
                }
                SvmExitCode::MSR => {
                    self.advance_rip(2)?;
                    if exit_info.exit_info_1 == 0 {
                        AxVCpuExitReason::SysRegRead {
                            addr: SysRegAddr::new(self.regs().rcx as _),
                            reg: 0,
                        }
                    } else {
                        AxVCpuExitReason::SysRegWrite {
                            addr: SysRegAddr::new(self.regs().rcx as _),
                            value: self.read_edx_eax(),
                        }
                    }
                }
                SvmExitCode::NPF => {
                    let info = self.nested_page_fault_info()?;
                    AxVCpuExitReason::NestedPageFault {
                        addr: info.fault_guest_paddr,
                        access_flags: info.access_flags,
                    }
                }
                // SVM INTR exits do not provide a usable vector here; return to
                // the scheduler and let normal host interrupt handling proceed.
                SvmExitCode::INTR => AxVCpuExitReason::Nothing,
                SvmExitCode::HLT => AxVCpuExitReason::Halt,
                SvmExitCode::SHUTDOWN => AxVCpuExitReason::SystemDown,
                _ => {
                    warn!("SVM unsupported VM-exit: {exit_info:#x?}");
                    warn!("VCpu {self:#x?}");
                    AxVCpuExitReason::Halt
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

    fn inject_interrupt(&mut self, _vector: usize) -> AxResult {
        ax_err!(
            Unsupported,
            "AMD SVM interrupt injection is not implemented yet"
        )
    }

    fn set_return_value(&mut self, val: usize) {
        self.regs_mut().rax = val as u64;
    }
}
