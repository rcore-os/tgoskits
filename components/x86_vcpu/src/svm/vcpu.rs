use alloc::collections::VecDeque;
use core::{
    arch::asm,
    fmt::{Debug, Formatter, Result as FmtResult},
    mem::size_of,
};

use ax_errno::{AxResult, ax_err, ax_err_type};
use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axvcpu::{AxArchVCpu, AxVCpuExitReason};
use axvisor_api::vmm::{VCpuId, VMId};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use x86_64::registers::control::Cr0Flags;

use super::{
    definitions::SvmIntercept,
    structs::{IOPm, MSRPm, VmcbFrame},
    vmcb::{NestedCtl, VmcbTlbControl, set_vmcb_segment},
};
use crate::{msr::Msr, regs::GeneralRegisters, xstate::XState};

const EFER_SVME: u64 = 1 << 12;
const EFER_LMA: u64 = 1 << 10;
const CR0_PE: u64 = 1 << 0;

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

/// Host state touched by SVM VMLOAD/VMSAVE and restored around guest entry.
#[derive(Default)]
pub struct VmLoadSaveStates {
    pub fs_base: u64,
    pub gs_base: u64,
    pub kernel_gs_base: u64,
    pub sysenter_cs: u64,
    pub sysenter_esp: u64,
    pub sysenter_eip: u64,
    pub star: u64,
    pub lstar: u64,
    pub cstar: u64,
    pub sfmask: u64,
    pub ldtr: u16,
    pub tr: u16,
}

impl VmLoadSaveStates {
    pub fn save_fs_gs(&mut self) {
        self.fs_base = Msr::IA32_FS_BASE.read();
        self.gs_base = Msr::IA32_GS_BASE.read();
        self.kernel_gs_base = Msr::IA32_KERNEL_GSBASE.read();
    }

    pub fn save_sysenter(&mut self) {
        self.sysenter_cs = Msr::IA32_SYSENTER_CS.read();
        self.sysenter_esp = Msr::IA32_SYSENTER_ESP.read();
        self.sysenter_eip = Msr::IA32_SYSENTER_EIP.read();
    }

    pub fn save_syscall(&mut self) {
        self.star = Msr::IA32_STAR.read();
        self.lstar = Msr::IA32_LSTAR.read();
        self.cstar = Msr::IA32_CSTAR.read();
        self.sfmask = Msr::IA32_FMASK.read();
    }

    pub fn save_segs(&mut self) {
        unsafe {
            asm!(
                "sldt {ldtr:x}",
                "str {tr:x}",
                ldtr = out(reg) self.ldtr,
                tr = out(reg) self.tr,
            );
        }
    }

    pub fn save_all(&mut self) {
        self.save_fs_gs();
        self.save_sysenter();
        self.save_syscall();
        self.save_segs();
    }

    pub fn load_fs_gs(&self) {
        unsafe {
            Msr::IA32_FS_BASE.write(self.fs_base);
            Msr::IA32_GS_BASE.write(self.gs_base);
            Msr::IA32_KERNEL_GSBASE.write(self.kernel_gs_base);
        }
    }

    pub fn load_sysenter(&self) {
        unsafe {
            Msr::IA32_SYSENTER_CS.write(self.sysenter_cs);
            Msr::IA32_SYSENTER_ESP.write(self.sysenter_esp);
            Msr::IA32_SYSENTER_EIP.write(self.sysenter_eip);
        }
    }

    pub fn load_syscall(&self) {
        unsafe {
            Msr::IA32_STAR.write(self.star);
            Msr::IA32_LSTAR.write(self.lstar);
            Msr::IA32_CSTAR.write(self.cstar);
            Msr::IA32_FMASK.write(self.sfmask);
        }
    }

    pub fn load_segs(&self) {
        unsafe {
            asm!(
                "lldt {ldtr:x}",
                "ltr {tr:x}",
                ldtr = in(reg) self.ldtr,
                tr = in(reg) self.tr,
            );
        }
    }

    pub fn load_all(&self) {
        self.load_fs_gs();
        self.load_sysenter();
        self.load_syscall();
        self.load_segs();
    }
}

/// AMD SVM vCPU skeleton.
///
/// The structure owns the hardware backing objects migrated from the old SVM
/// prototype, but `run()` is intentionally left unsupported until NPT and exit
/// handling are adapted to the current AxVisor interfaces.
#[repr(C)]
pub struct SvmVcpu {
    guest_regs: GeneralRegisters,
    // Used by `svm_run()` assembly; keep immediately after `guest_regs`.
    host_stack_top: u64,
    launched: bool,
    entry: Option<GuestPhysAddr>,
    npt_root: Option<HostPhysAddr>,
    vmcb: VmcbFrame,
    load_save_states: VmLoadSaveStates,
    iopm: IOPm,
    msrpm: MSRPm,
    pending_events: VecDeque<(u8, Option<u32>)>,
    xstate: XState,
}

impl SvmVcpu {
    fn create(_vm_id: VMId, _vcpu_id: VCpuId) -> AxResult<Self> {
        let vcpu = Self {
            guest_regs: GeneralRegisters::default(),
            host_stack_top: 0,
            launched: false,
            entry: None,
            npt_root: None,
            vmcb: VmcbFrame::new()?,
            load_save_states: VmLoadSaveStates::default(),
            iopm: IOPm::passthrough_all()?,
            msrpm: MSRPm::passthrough_all()?,
            pending_events: VecDeque::with_capacity(8),
            xstate: XState::new(),
        };
        info!("[HV] created SvmVcpu(vmcb: {:#x})", vcpu.vmcb.phys_addr());
        Ok(vcpu)
    }

    fn setup_vmcb(&mut self, entry: GuestPhysAddr, npt_root: HostPhysAddr) -> AxResult {
        self.setup_vmcb_guest(entry)?;
        self.setup_vmcb_control(npt_root)
    }

    fn setup_vmcb_guest(&mut self, entry: GuestPhysAddr) -> AxResult {
        let cr0_val =
            Cr0Flags::NOT_WRITE_THROUGH | Cr0Flags::CACHE_DISABLE | Cr0Flags::EXTENSION_TYPE;
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        let state = &mut vmcb.state;

        state.cr0.set(cr0_val.bits());
        state.cr3.set(0);
        state.cr4.set(0);

        state.cs.selector.set(0);
        state.cs.base.set(0);
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
        state.rip.set(entry.as_usize() as u64);
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

        for intercept in [
            SvmIntercept::NMI,
            SvmIntercept::CPUID,
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

    fn bind_to_current_processor(&self) -> AxResult {
        Ok(())
    }

    fn unbind_from_current_processor(&self) -> AxResult {
        Ok(())
    }

    pub fn get_cpu_mode(&self) -> VmCpuMode {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
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
        unsafe { self.vmcb.as_vmcb().exit_info() }
    }

    pub fn regs(&self) -> &GeneralRegisters {
        &self.guest_regs
    }

    pub fn regs_mut(&mut self) -> &mut GeneralRegisters {
        &mut self.guest_regs
    }

    pub fn stack_pointer(&self) -> usize {
        unsafe { self.vmcb.as_vmcb().state.rsp.get() as usize }
    }

    pub fn set_stack_pointer(&mut self, rsp: usize) {
        unsafe { self.vmcb.as_vmcb().state.rsp.set(rsp as u64) };
    }

    pub fn set_cr(&mut self, cr_idx: usize, val: u64) -> AxResult {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        match cr_idx {
            0 => vmcb.state.cr0.set(val),
            3 => vmcb.state.cr3.set(val),
            4 => vmcb.state.cr4.set(val),
            _ => return ax_err!(InvalidInput, format_args!("Unsupported CR{}", cr_idx)),
        }
        Ok(())
    }

    fn cr(&self, cr_idx: usize) -> usize {
        let vmcb = unsafe { self.vmcb.as_vmcb() };
        match cr_idx {
            0 => vmcb.state.cr0.get() as usize,
            3 => vmcb.state.cr3.get() as usize,
            4 => vmcb.state.cr4.get() as usize,
            _ => unreachable!(),
        }
    }

    fn load_guest_xstate(&mut self) {
        self.xstate.switch_to_guest();
    }

    fn load_host_xstate(&mut self) {
        self.xstate.switch_to_host();
    }

    fn before_vmrun(&mut self) {
        unsafe {
            super::instructions::clgi();
            self.vmcb.as_vmcb().state.rax.set(self.regs().rax);
        }
        self.load_save_states.save_fs_gs();
        unsafe {
            let _ = super::instructions::vmload(self.vmcb.phys_addr().as_usize() as u64);
        }
    }

    fn after_vmrun(&mut self) {
        unsafe {
            let _ = super::instructions::vmsave(self.vmcb.phys_addr().as_usize() as u64);
        }
        self.load_save_states.load_fs_gs();
        self.regs_mut().rax = unsafe { self.vmcb.as_vmcb().state.rax.get() };
        unsafe {
            super::instructions::stgi();
        }
    }

    pub unsafe fn svm_run(&mut self) {
        let self_addr = self as *mut Self as u64;
        let vmcb = self.vmcb.phys_addr().as_usize() as u64;

        self.load_guest_xstate();
        self.before_vmrun();

        unsafe {
            asm!(
                save_regs_no_rax!(),
                "mov [rdi + {host_stack_top}], rsp",
                "mov rsp, rdi",
                restore_regs_no_rax!(),
                "vmrun rax",
                save_regs_no_rax!(),
                "mov rdi, rsp",
                "mov rsp, [rdi + {host_stack_top}]",
                restore_regs_no_rax!(),
                host_stack_top = const size_of::<GeneralRegisters>(),
                in("rax") vmcb,
                in("rdi") self_addr,
            );
        }

        self.after_vmrun();
        self.load_host_xstate();
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
    type SetupConfig = ();

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
        ax_err!(Unsupported, "AMD SVM vCPU execution is not implemented yet")
    }

    fn bind(&mut self) -> AxResult {
        self.bind_to_current_processor()
    }

    fn unbind(&mut self) -> AxResult {
        self.launched = false;
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
