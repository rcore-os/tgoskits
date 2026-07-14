use core::marker::PhantomData;

use ax_cpu_local::CpuPin;

use crate::{
    context_frame::LoongArchContextFrame,
    exception::{handle_exception_irq, handle_exception_sync},
    host::LoongArchHostOps,
    iocsr::{
        LoongArchIocsrStateRef, init_guest_iocsr, inject_enabled_pending_interrupt,
        inject_guest_eiointc_vector,
    },
    registers::{
        CSR_ASID, CSR_CRMD, CSR_ECFG, CSR_KSAVE_KSP, CSR_PGDH, CSR_PGDL, CSR_PRMD, CSR_PWCH,
        CSR_PWCL, CSR_STLBPS, CSR_TLBRENTRY, INT_IPI, INT_TIMER, csr_read, csr_write,
        gcfg_set_gpm_num, gcfg_set_matc, gcfg_set_toci, gcfg_set_toe, gcfg_set_tohu, gcfg_set_top,
        gcfg_set_topi, gcfg_set_toti, gstat_set_gid, gstat_set_pgm, gtlbc_set_tgid,
        gtlbc_set_use_tgid, set_prmd_pie,
    },
    trap::{TrapKind, current_badi},
    types::{
        LoongArchAccessFlags, LoongArchGuestPhysAddr, LoongArchHostPhysAddr, LoongArchHostVirtAddr,
        LoongArchNestedPagingConfig, LoongArchVcpuError, LoongArchVcpuId, LoongArchVcpuResult,
        LoongArchVmExit, LoongArchVmId,
    },
};

unsafe extern "C" {
    fn _run_guest(ctx: *mut LoongArchContextFrame) -> !;
    fn _guest_tlb_refill_vector();
    static _exception_vectors: u8;
}

const GUEST_RESET_CRMD_DIRECT: usize = 1 << 3;
const GUEST_BOOT_PRMD: usize = 1 << 2;
const GUEST_DMW_DA_BITS: usize = 48;
const GUEST_DMW_PLV0: usize = 1 << 0;
const GUEST_DMW_MAT_CC: usize = 1 << 4;
const GUEST_BOOT_VSEG: usize = 0x9000;
const GUEST_BOOT_DMW: usize =
    (GUEST_BOOT_VSEG << GUEST_DMW_DA_BITS) | GUEST_DMW_PLV0 | GUEST_DMW_MAT_CC;
const CSR_CRMD_IE: usize = 1 << 2;
const LOCAL_INTERRUPT_MASK: usize = (1 << (INT_IPI + 1)) - 1;
const HOST_DMW_CACHED_BASE: usize = 0x9000_0000_0000_0000;

#[derive(Clone, Copy, Debug, Default)]
struct HostTranslationState {
    crmd: usize,
    prmd: usize,
    ksave_ksp: usize,
    guest_exit_eentry: usize,
    ecfg: usize,
    pgdl: usize,
    pgdh: usize,
    pwcl: usize,
    pwch: usize,
    stlbps: usize,
    tlbrentry: usize,
    asid: usize,
}

#[derive(Clone, Debug)]
pub struct LoongArchVCpuCreateConfig {
    pub cpu_id: usize,
    pub dtb_addr: usize,
    pub boot_args: [usize; 3],
    pub boot_stack_top: usize,
    pub firmware_boot: bool,
    pub iocsr_state: LoongArchIocsrStateRef,
}

#[derive(Clone, Debug, Default)]
pub struct LoongArchVCpuSetupConfig {
    pub passthrough_interrupt: bool,
    pub passthrough_timer: bool,
    pub boot_args: [usize; 3],
    pub boot_stack_top: usize,
    pub firmware_boot: bool,
}

#[repr(C)]
#[derive(Debug)]
pub struct LoongArchVcpu<H: LoongArchHostOps> {
    ctx: LoongArchContextFrame,
    #[allow(dead_code)]
    host_stack_top: usize,
    stage2_root: LoongArchHostPhysAddr,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    cpu_id: usize,
    iocsr_state: LoongArchIocsrStateRef,
    guest_timer_token: Option<usize>,
    last_badi: usize,
    entry_logged: bool,
    host_translation_state: HostTranslationState,
    _host: PhantomData<fn() -> H>,
}

pub type LoongArchVCpu<H> = LoongArchVcpu<H>;

impl<H: LoongArchHostOps + 'static> LoongArchVcpu<H> {
    pub fn new(
        vm_id: LoongArchVmId,
        vcpu_id: LoongArchVcpuId,
        config: LoongArchVCpuCreateConfig,
    ) -> LoongArchVcpuResult<Self> {
        let mut ctx = LoongArchContextFrame::default();
        if config.firmware_boot {
            // Firmware reset entry keeps the initial argument registers in their
            // reset state. The firmware obtains its early FDT via its own reset
            // path, while OS direct boot receives DTB/EFI args from AxVM.
        } else if config.boot_args != [0; 3] {
            ctx.set_argument(config.boot_args[0]);
            ctx.set_a1(config.boot_args[1]);
            ctx.set_a2(config.boot_args[2]);
            if config.boot_stack_top != 0 {
                ctx.set_gpr(3, config.boot_stack_top);
            }
        } else {
            ctx.set_argument(config.cpu_id);
            ctx.set_a1(config.dtb_addr);
        }

        Ok(Self {
            ctx,
            host_stack_top: 0,
            stage2_root: LoongArchHostPhysAddr::from_usize(0),
            vm_id,
            vcpu_id,
            cpu_id: config.cpu_id,
            iocsr_state: config.iocsr_state,
            guest_timer_token: None,
            last_badi: 0,
            entry_logged: false,
            host_translation_state: HostTranslationState::default(),
            _host: PhantomData,
        })
    }

    pub fn set_entry(&mut self, entry: LoongArchGuestPhysAddr) -> LoongArchVcpuResult {
        self.ctx.sepc = entry.as_usize();
        self.ctx.gcsr_era = entry.as_usize();
        Ok(())
    }

    pub fn set_nested_page_table(
        &mut self,
        config: LoongArchNestedPagingConfig,
    ) -> LoongArchVcpuResult {
        self.stage2_root = config.root_paddr;
        Ok(())
    }

    pub fn setup(&mut self, config: LoongArchVCpuSetupConfig) -> LoongArchVcpuResult {
        if !config.firmware_boot && config.boot_args != [0; 3] {
            self.ctx.set_argument(config.boot_args[0]);
            self.ctx.set_a1(config.boot_args[1]);
            self.ctx.set_a2(config.boot_args[2]);
            if config.boot_stack_top != 0 {
                self.ctx.set_gpr(3, config.boot_stack_top);
            }
        }
        self.init_hv(config);
        Ok(())
    }

    pub fn run(&mut self, cpu_pin: &CpuPin) -> LoongArchVcpuResult<LoongArchVmExit> {
        unsafe {
            self.enable_guest_mode();
        }
        if inject_enabled_pending_interrupt(cpu_pin, &self.iocsr_state, &mut self.ctx, self.vcpu_id)
        {
            log::trace!(
                "LoongArch guest pending interrupt injected before entry: VM[{}] VCpu[{}] \
                 sepc={:#x}, era={:#x}, estat={:#x}, ecfg={:#x}",
                self.vm_id,
                self.vcpu_id,
                self.ctx.sepc,
                self.ctx.gcsr_era,
                self.ctx.gcsr_estat,
                self.ctx.gcsr_ectl
            );
        }
        let exit_reason = unsafe { self.run_guest() };

        log::trace!(
            "LoongArch guest exit raw: VM[{}] VCpu[{}] reason={}, sepc={:#x}, gera={:#x}",
            self.vm_id,
            self.vcpu_id,
            exit_reason,
            self.ctx.sepc,
            self.ctx.gcsr_era
        );
        let trap_kind =
            TrapKind::try_from(exit_reason as u8).map_err(|_| LoongArchVcpuError::BadState)?;
        self.vmexit_handler(cpu_pin, trap_kind)
    }

    pub fn bind(&mut self, _cpu_pin: &CpuPin) -> LoongArchVcpuResult {
        let _ = self.cpu_id;
        unsafe {
            self.save_host_translation_state();
        }
        Ok(())
    }

    pub fn unbind(&mut self, _cpu_pin: &CpuPin) -> LoongArchVcpuResult {
        unsafe {
            self.restore_host_translation_state();
        }
        Ok(())
    }

    pub fn set_gpr(&mut self, idx: usize, val: usize) {
        self.ctx.set_gpr(idx, val);
    }

    pub fn decode_mmio_fault(
        &mut self,
        fault_addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    ) -> Option<LoongArchVmExit> {
        let gcsr_badi = self.ctx.gcsr_badi;
        let exit =
            crate::mmio::decode_mmio_fault(&mut self.ctx, self.last_badi, fault_addr, access_flags)
                .or_else(|| {
                    if gcsr_badi == self.last_badi {
                        None
                    } else {
                        crate::mmio::decode_mmio_fault(
                            &mut self.ctx,
                            gcsr_badi,
                            fault_addr,
                            access_flags,
                        )
                    }
                });

        if exit.is_none() {
            let (rj, rj_value, normal_addr, ptr_addr) =
                crate::mmio::describe_mmio_fault(&self.ctx, self.last_badi);
            log::debug!(
                "LoongArch MMIO decode failed: addr={:#x}, flags={:?}, sepc={:#x}, badi={:#x}, \
                 gcsr_badi={:#x}, rj={}, rj_value={:#x}, normal_addr={:#x}, ptr_addr={:#x}",
                fault_addr.as_usize(),
                access_flags,
                self.ctx.sepc,
                self.last_badi,
                gcsr_badi,
                rj,
                rj_value,
                normal_addr,
                ptr_addr
            );
        }

        exit
    }

    pub fn inject_interrupt(&mut self, vector: usize) -> LoongArchVcpuResult {
        if vector <= INT_IPI {
            self.ctx.gcsr_estat |= 1usize << vector;
        } else if let Some(hwi) =
            inject_guest_eiointc_vector(&self.iocsr_state, self.vm_id, self.vcpu_id, vector)
        {
            self.ctx.gcsr_estat |= 1usize << hwi;
        } else {
            log::warn!("Ignoring unsupported LoongArch interrupt vector {vector}");
        }
        Ok(())
    }

    pub fn set_return_value(&mut self, val: usize) {
        self.ctx.set_a0(val);
    }

    pub fn inject_external_interrupt(
        &mut self,
        vector: usize,
        physical_irq: usize,
    ) -> LoongArchVcpuResult {
        if let Some(hwi) =
            inject_guest_eiointc_vector(&self.iocsr_state, self.vm_id, self.vcpu_id, vector)
        {
            self.ctx.gcsr_estat |= 1usize << hwi;
            log::debug!(
                "LoongArch guest external IRQ pending: VM[{}] VCpu[{}] physical_irq={}, \
                 eiointc_hwi={}, routed_vector={}",
                self.vm_id,
                self.vcpu_id,
                physical_irq,
                hwi,
                vector
            );
            return self.inject_interrupt(hwi);
        }
        self.inject_interrupt(vector)
    }

    pub fn has_enabled_pending_interrupt(&self) -> bool {
        self.ctx.gcsr_eentry != 0
            && self.ctx.gcsr_crmd & CSR_CRMD_IE != 0
            && self.ctx.gcsr_estat & self.ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK != 0
    }

    pub fn idle_wait_timeout(&self) -> core::time::Duration {
        const TIMER_ENABLE: usize = 1 << 0;
        const TIMER_INIT_MASK: usize = !0x3;
        const MIN_WAIT: core::time::Duration = core::time::Duration::from_micros(50);
        const MAX_WAIT: core::time::Duration = core::time::Duration::from_millis(10);

        if self.ctx.gcsr_ectl & (1usize << INT_TIMER) == 0 || self.ctx.gcsr_tcfg & TIMER_ENABLE == 0
        {
            return MAX_WAIT;
        }

        let ticks = if self.ctx.gcsr_tval == 0 {
            self.ctx.gcsr_tcfg & TIMER_INIT_MASK
        } else {
            self.ctx.gcsr_tval
        };
        if ticks == 0 {
            return MIN_WAIT;
        }

        let nanos = H::ticks_to_nanos(ticks as u64);
        core::time::Duration::from_nanos(nanos).clamp(MIN_WAIT, MAX_WAIT)
    }

    fn init_hv(&mut self, config: LoongArchVCpuSetupConfig) {
        self.init_vm_context(config);
        init_guest_iocsr(&self.iocsr_state, self.vcpu_id);
    }

    fn init_vm_context(&mut self, config: LoongArchVCpuSetupConfig) {
        self.init_guest_boot_state();
        self.init_guest_page_table_state();
        self.init_guest_exception_state();

        self.ctx.gcsr_asid = self.vcpu_id;
        self.ctx.gcsr_cpuid = self.cpu_id;

        let _ = config.passthrough_timer;
    }

    /// Set guest architectural boot state: CRMD (DA mode), PRMD (PIE), EUEN, DMW0-3.
    fn init_guest_boot_state(&mut self) {
        self.ctx.gcsr_crmd = GUEST_RESET_CRMD_DIRECT;
        self.ctx.gcsr_prmd = GUEST_BOOT_PRMD;
        self.ctx.gcsr_euen = 0;
        self.ctx.gcsr_dmw0 = GUEST_BOOT_DMW;
        self.ctx.gcsr_dmw1 = 0;
        self.ctx.gcsr_dmw2 = 0;
        self.ctx.gcsr_dmw3 = 0;
    }

    /// Zero guest page-table CSRs. Guest starts in DA mode (CRMD.DA=1)
    /// with no active page table; it programs these itself when enabling paging.
    fn init_guest_page_table_state(&mut self) {
        self.ctx.gcsr_pgdl = 0;
        self.ctx.gcsr_pgdh = 0;
        self.ctx.gcsr_pgd = 0;
        self.ctx.gcsr_pwcl = 0;
        self.ctx.gcsr_pwch = 0;
        self.ctx.gcsr_stlbps = 0;
    }

    /// Zero guest exception vectors. The guest programs its own EENTRY
    /// and TLBRENTRY early in boot before any exception can occur.
    fn init_guest_exception_state(&mut self) {
        self.ctx.gcsr_eentry = 0;
        self.ctx.gcsr_tlbrentry = 0;
        self.ctx.gcsr_tlbrprmd = 0;
        self.ctx.gcsr_tlbrera = 0;
    }

    fn host_dmw_alias(vaddr: usize) -> usize {
        HOST_DMW_CACHED_BASE | H::virt_to_phys(LoongArchHostVirtAddr::from_usize(vaddr)).as_usize()
    }

    /// Save host translation CSRs before switching to stage2.
    unsafe fn save_host_translation_state(&mut self) {
        let state = HostTranslationState {
            crmd: csr_read::<CSR_CRMD>(),
            prmd: csr_read::<CSR_PRMD>(),
            ksave_ksp: csr_read::<CSR_KSAVE_KSP>(),
            guest_exit_eentry: csr_read::<{ crate::registers::CSR_EENTRY }>(),
            ecfg: csr_read::<CSR_ECFG>(),
            pgdl: csr_read::<CSR_PGDL>(),
            pgdh: csr_read::<CSR_PGDH>(),
            pwcl: csr_read::<CSR_PWCL>(),
            pwch: csr_read::<CSR_PWCH>(),
            stlbps: csr_read::<CSR_STLBPS>(),
            tlbrentry: csr_read::<CSR_TLBRENTRY>(),
            asid: csr_read::<CSR_ASID>(),
        };
        self.host_translation_state = state;
        self.ctx.host_pgdl = state.pgdl;
        self.ctx.host_pgdh = state.pgdh;
        self.ctx.host_pwcl = state.pwcl;
        self.ctx.host_pwch = state.pwch;
        self.ctx.host_stlbps = state.stlbps;
        self.ctx.host_tlbrentry = state.tlbrentry;
        self.ctx.host_asid = state.asid;
        self.ctx.host_eentry = state.guest_exit_eentry;
        self.ctx.host_ecfg = state.ecfg;
        self.ctx.guest_tlbrentry =
            Self::host_dmw_alias(_guest_tlb_refill_vector as *const () as usize);
        self.ctx.guest_eentry =
            Self::host_dmw_alias(core::ptr::addr_of!(_exception_vectors) as usize);
    }

    /// Enable LVZ guest-mode hardware: GID, TGID, PGM, GCFG, GINTC, PRMD.PIE.
    unsafe fn enable_guest_mode(&self) {
        let guest_id = self.vm_id + 1;
        gstat_set_gid(guest_id);
        gstat_set_pgm(true);
        gtlbc_set_use_tgid(true);
        gtlbc_set_tgid(guest_id);
        gcfg_set_matc(0x1);
        gcfg_set_topi(false);
        gcfg_set_toti(false);
        gcfg_set_toe(false);
        gcfg_set_top(false);
        gcfg_set_tohu(false);
        gcfg_set_toci(0x2);
        gcfg_set_gpm_num(0);
        set_prmd_pie(true);
    }

    /// Restore host translation CSRs from the vCPU-owned saved state.
    unsafe fn restore_host_translation_state(&self) {
        let state = self.host_translation_state;
        csr_write::<{ crate::registers::CSR_EENTRY }>(state.guest_exit_eentry);
        csr_write::<CSR_ECFG>(state.ecfg);
        csr_write::<CSR_PGDL>(state.pgdl);
        csr_write::<CSR_PGDH>(state.pgdh);
        csr_write::<CSR_PWCL>(state.pwcl);
        csr_write::<CSR_PWCH>(state.pwch);
        csr_write::<CSR_STLBPS>(state.stlbps);
        csr_write::<CSR_TLBRENTRY>(state.tlbrentry);
        csr_write::<CSR_ASID>(state.asid);
        csr_write::<CSR_KSAVE_KSP>(state.ksave_ksp);
        csr_write::<CSR_PRMD>(state.prmd);
        csr_write::<CSR_CRMD>(state.crmd);
        core::arch::asm!("invtlb 0x0, $r0, $r0");
    }

    #[unsafe(naked)]
    unsafe extern "C" fn run_guest(&mut self) -> usize {
        core::arch::naked_asm!(
            "addi.d $sp, $sp, -14 * 8",
            "st.d $ra, $sp, 0",
            "st.d $s0, $sp, 8",
            "st.d $s1, $sp, 16",
            "st.d $s2, $sp, 24",
            "st.d $s3, $sp, 32",
            "st.d $s4, $sp, 40",
            "st.d $s5, $sp, 48",
            "st.d $s6, $sp, 56",
            "st.d $s7, $sp, 64",
            "st.d $s8, $sp, 72",
            "st.d $fp, $sp, 80",
            "st.d $tp, $sp, 88",
            // r21 is the physical CPU's per-CPU base. Guest exit restores it
            // from the KS3 shadow instead of treating it as vCPU task state.
            "move $s0, $a0",
            "move $t0, $sp",
            "addi.d $t1, $a0, {host_stack_top_offset}",
            "st.d $t0, $t1, 0",
            "la.pcrel $a0, {run_guest_asm}",
            "bl {host_dmw_alias}",
            "move $t0, $a0",
            "move $a0, $s0",
            "addi.d $a1, $a0, {stage2_root_offset}",
            "ld.d $a1, $a1, 0",
            "jirl $ra, $t0, 0",
            "bl {run_guest_panic}",
            host_stack_top_offset = const core::mem::size_of::<crate::context_frame::LoongArchContextFrame>(),
            stage2_root_offset = const core::mem::size_of::<crate::context_frame::LoongArchContextFrame>() + core::mem::size_of::<usize>(),
            run_guest_asm = sym _run_guest,
            host_dmw_alias = sym Self::host_dmw_alias_for_asm,
            run_guest_panic = sym Self::run_guest_panic,
        );
    }

    extern "C" fn host_dmw_alias_for_asm(ptr: usize) -> usize {
        Self::host_dmw_alias(ptr)
    }

    unsafe fn run_guest_panic() -> ! {
        panic!("run_guest_panic: control returned to run_guest");
    }

    fn vmexit_handler(
        &mut self,
        cpu_pin: &CpuPin,
        exit_reason: TrapKind,
    ) -> LoongArchVcpuResult<LoongArchVmExit> {
        self.last_badi = if self.ctx.host_badi != 0 {
            self.ctx.host_badi
        } else {
            current_badi()
        };

        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync::<H>(
                cpu_pin,
                &self.iocsr_state,
                &mut self.ctx,
                self.vm_id,
                self.vcpu_id,
                &mut self.guest_timer_token,
            ),
            TrapKind::Irq => handle_exception_irq(&mut self.ctx),
        }
    }
}
