use ax_errno::AxResult;
#[cfg(not(target_arch = "loongarch64"))]
use ax_errno::ax_err;
#[cfg(target_arch = "loongarch64")]
use ax_memory_addr::VirtAddr;
#[cfg(target_arch = "loongarch64")]
use ax_page_table_multiarch::loongarch64::LA64MetaData;
use axvcpu::{AxArchVCpu, AxVCpuExitReason, GuestPhysAddr, HostPhysAddr, VCpuId, VMId};
#[cfg(target_arch = "loongarch64")]
use loongArch64::register::prmd;

#[cfg(target_arch = "loongarch64")]
use crate::exception::{TrapKind, handle_exception_irq, handle_exception_sync};
use crate::{
    context_frame::LoongArchContextFrame,
    host,
    registers::{
        CSR_PGDH, CSR_PGDL, CSR_PWCH, CSR_PWCL, CSR_STLBPS, CSR_TLBRENTRY, INT_TIMER, csr_read,
        csr_write, gcfg_set_gpm_num, gcfg_set_matc, gcfg_set_toci, gcfg_set_toe, gcfg_set_tohu,
        gcfg_set_top, gcfg_set_topi, gcfg_set_toti, get_ecfg_vs, gintc_set_hwip, gstat_set_gid,
        gstat_set_pgm, gtlbc_set_tgid, gtlbc_set_use_tgid, set_ecfg_line_enabled, set_ecfg_vs,
    },
};

#[cfg(target_arch = "loongarch64")]
unsafe extern "C" {
    fn _run_guest(ctx: *mut LoongArchContextFrame) -> !;
    static _exception_vectors: u8;
}

#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_SP: usize = 0;

#[cfg(target_arch = "loongarch64")]
unsafe fn save_host_sp() {
    let sp: usize;
    core::arch::asm!("move {0}, $sp", out(reg) sp);
    HOST_SP.write_current_raw(sp);
}

#[repr(C)]
#[derive(Debug)]
pub struct LoongArchVCpu {
    ctx: LoongArchContextFrame,
    #[allow(dead_code)]
    host_stack_top: usize,
    stage2_root: HostPhysAddr,
    vm_id: VMId,
    vcpu_id: VCpuId,
    cpu_id: usize,
}

#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_STAGE2_PGDL: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_STAGE2_PGDH: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_STAGE2_PWCL: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_STAGE2_PWCH: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_STAGE2_STLBPS: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_STAGE2_TLBRENTRY: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_GUEST_EXIT_EENTRY: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_ECFG_VS: usize = 0;

/// Host CSR number for TCFG (Timer Configuration).
#[cfg(target_arch = "loongarch64")]
const CSR_HOST_TCFG: u16 = 0x41;

/// TCFG.EN bit: timer enable.
#[cfg(target_arch = "loongarch64")]
const TCFG_EN: usize = 0x1;

#[cfg(target_arch = "loongarch64")]
const GUEST_RESET_CRMD_DIRECT: usize = 1 << 3;
#[cfg(target_arch = "loongarch64")]
const GUEST_BOOT_PRMD: usize = 1 << 2;
#[cfg(target_arch = "loongarch64")]
const GUEST_DMW_DA_BITS: usize = 48;
#[cfg(target_arch = "loongarch64")]
const GUEST_DMW_PLV0: usize = 1 << 0;
#[cfg(target_arch = "loongarch64")]
const GUEST_DMW_MAT_CC: usize = 1 << 4;
#[cfg(target_arch = "loongarch64")]
const GUEST_BOOT_VSEG: usize = 0x9000;
#[cfg(target_arch = "loongarch64")]
const GUEST_BOOT_DMW: usize =
    (GUEST_BOOT_VSEG << GUEST_DMW_DA_BITS) | GUEST_DMW_PLV0 | GUEST_DMW_MAT_CC;

#[derive(Clone, Debug, Default)]
pub struct LoongArchVCpuCreateConfig {
    pub cpu_id: usize,
    pub dtb_addr: usize,
}

#[derive(Clone, Debug, Default)]
pub struct LoongArchVCpuSetupConfig {
    pub passthrough_interrupt: bool,
    pub passthrough_timer: bool,
}

impl AxArchVCpu for LoongArchVCpu {
    type CreateConfig = LoongArchVCpuCreateConfig;
    type SetupConfig = LoongArchVCpuSetupConfig;

    fn new(_vm_id: usize, _vcpu_id: usize, config: Self::CreateConfig) -> AxResult<Self> {
        let mut ctx = LoongArchContextFrame::default();
        ctx.set_argument(config.cpu_id);
        ctx.set_a1(config.dtb_addr);

        Ok(Self {
            ctx,
            host_stack_top: 0,
            stage2_root: HostPhysAddr::from_usize(0),
            vm_id: _vm_id,
            vcpu_id: _vcpu_id,
            cpu_id: config.cpu_id,
        })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.ctx.sepc = entry.as_usize();
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.stage2_root = ept_root;
        Ok(())
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        self.init_hv(config);
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        #[cfg(target_arch = "loongarch64")]
        {
            // Before entering the guest, program the host hardware timer with the
            // guest's requested TCFG value.  In LVZ the guest's timer CSR writes go
            // to GCSR TCFG transparently (no GSPR trap), so the only place we can
            // observe the guest's timer configuration is in ctx.gcsr_tcfg, which is
            // updated by SAVE_GUEST_REGS on each VM exit.
            self.sync_guest_timer_to_host();

            log::debug!("LoongArch guest entry context:\n{}", self.ctx);
            let exit_reason = unsafe {
                save_host_sp();
                self.run_guest()
            };

            let trap_kind = TrapKind::try_from(exit_reason as u8).expect("invalid TrapKind");
            self.vmexit_handler(trap_kind)
        }

        #[cfg(not(target_arch = "loongarch64"))]
        {
            ax_err!(
                Unsupported,
                "LoongArch guest entry is only available on loongarch64 hosts"
            )
        }
    }

    fn bind(&mut self) -> AxResult {
        let _ = self.cpu_id;
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            self.save_host_translation_state();
            self.activate_stage2_walk_state();
            self.install_host_exit_vectors();
            // Our guest-exit vector table uses 0x80-byte spacing between entries,
            // i.e. 32 instructions, so CSR.ECFG.VS must be 5.
            set_ecfg_vs(5);
            self.enable_guest_mode();
        }
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            set_ecfg_line_enabled(INT_TIMER, true);
            self.restore_host_translation_state();
        }
        Ok(())
    }

    fn set_gpr(&mut self, idx: usize, val: usize) {
        self.ctx.set_gpr(idx, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        crate::registers::inject_interrupt(vector);
        Ok(())
    }

    fn set_return_value(&mut self, val: usize) {
        self.ctx.set_a0(val);
    }
}

impl LoongArchVCpu {
    fn init_hv(&mut self, config: LoongArchVCpuSetupConfig) {
        self.init_vm_context(config);
    }

    fn init_vm_context(&mut self, config: LoongArchVCpuSetupConfig) {
        #[cfg(target_arch = "loongarch64")]
        {
            self.init_guest_boot_state();
            self.init_guest_page_table_state();
            self.init_guest_exception_state();
        }

        self.ctx.gcsr_asid = self.vcpu_id;
        self.ctx.gcsr_cpuid = self.cpu_id;

        if config.passthrough_timer {
            self.ctx.gcsr_tcfg = 0x1;
        }
    }

    /// Program the host hardware timer to match the guest's requested timer
    /// configuration. In LVZ, guest timer CSR writes (TCFG/TVAL/TICLR) are
    /// transparent (GCSR-swappable) and do NOT trap, so the guest's virtual
    /// timer state is only observable via `ctx.gcsr_tcfg` which is updated by
    /// SAVE_GUEST_REGS on each VM exit.
    ///
    /// By programming the host timer to match, the hardware timer fires at the
    /// guest's requested time, causing an IRQ VM exit. The IRQ handler then
    /// injects the timer interrupt into the guest via `gcsr_estat`.
    ///
    /// **Limitation**: This direct passthrough is only correct in the 1:1 vCPU
    /// model where each vCPU exclusively owns a physical CPU. In SMP or
    /// shared-CPU scenarios, a software-emulated timer (maintaining per-guest
    /// virtual TCFG/TVAL) will be needed.
    #[cfg(target_arch = "loongarch64")]
    fn sync_guest_timer_to_host(&self) {
        let guest_tcfg = self.ctx.gcsr_tcfg;
        // Only program the host timer when the guest has set a non-zero
        // init value.  With init_val=0 the timer fires immediately on
        // every VM entry, preventing the guest from making progress.
        let init_val = guest_tcfg >> 2;
        if guest_tcfg & TCFG_EN != 0 && init_val != 0 {
            log::debug!("Sync guest timer to host: gcsr_tcfg={:#x}", guest_tcfg);
            unsafe {
                csr_write::<CSR_HOST_TCFG>(guest_tcfg);
            }
        }
    }

    /// Set guest architectural boot state: CRMD (DA mode), PRMD (PIE), EUEN, DMW0-3.
    #[cfg(target_arch = "loongarch64")]
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
    #[cfg(target_arch = "loongarch64")]
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
    #[cfg(target_arch = "loongarch64")]
    fn init_guest_exception_state(&mut self) {
        self.ctx.gcsr_eentry = 0;
        self.ctx.gcsr_tlbrentry = 0;
        self.ctx.gcsr_tlbrprmd = 0;
        self.ctx.gcsr_tlbrera = 0;
    }

    /// Save host translation CSRs to per-CPU storage before switching to stage2.
    #[cfg(target_arch = "loongarch64")]
    unsafe fn save_host_translation_state(&self) {
        HOST_GUEST_EXIT_EENTRY.write_current_raw(csr_read::<{ crate::registers::CSR_EENTRY }>());
        HOST_ECFG_VS.write_current_raw(get_ecfg_vs());
        HOST_STAGE2_PGDL.write_current_raw(csr_read::<CSR_PGDL>());
        HOST_STAGE2_PGDH.write_current_raw(csr_read::<CSR_PGDH>());
        HOST_STAGE2_PWCL.write_current_raw(csr_read::<CSR_PWCL>());
        HOST_STAGE2_PWCH.write_current_raw(csr_read::<CSR_PWCH>());
        HOST_STAGE2_STLBPS.write_current_raw(csr_read::<CSR_STLBPS>());
        HOST_STAGE2_TLBRENTRY.write_current_raw(csr_read::<CSR_TLBRENTRY>());
    }

    /// Program stage2 page-walk CSRs from the VM's stage2 root.
    #[cfg(target_arch = "loongarch64")]
    unsafe fn activate_stage2_walk_state(&self) {
        let root = self.stage2_root.as_usize();
        csr_write::<CSR_PWCL>(LA64MetaData::PWCL_VALUE as usize);
        csr_write::<CSR_PWCH>(LA64MetaData::PWCH_VALUE as usize);
        csr_write::<CSR_STLBPS>(12);
        csr_write::<CSR_PGDL>(root);
        csr_write::<CSR_PGDH>(root);
    }

    /// Install host exit vector table (CSR.EENTRY) and TLB refill handler
    /// (CSR.TLBRENTRY), then flush stale TLB entries.
    #[cfg(target_arch = "loongarch64")]
    unsafe fn install_host_exit_vectors(&self) {
        unsafe extern "C" {
            fn handle_tlb_refill();
        }
        let tlbrentry_vaddr = VirtAddr::from_ptr_of(handle_tlb_refill as *const ());
        let tlbrentry_paddr = host::virt_to_phys(tlbrentry_vaddr).as_usize();
        let guest_exit_eentry = core::ptr::addr_of!(_exception_vectors) as usize;

        csr_write::<CSR_TLBRENTRY>(tlbrentry_paddr);
        csr_write::<{ crate::registers::CSR_EENTRY }>(guest_exit_eentry);
        core::arch::asm!("invtlb 0x0, $r0, $r0");
    }

    /// Enable LVZ guest-mode hardware: GID, TGID, PGM, GCFG, GINTC, PRMD.PIE.
    #[cfg(target_arch = "loongarch64")]
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
        gintc_set_hwip(0xff);
        set_ecfg_line_enabled(INT_TIMER, false);
        prmd::set_pie(true);
    }

    /// Restore host translation CSRs from per-CPU storage.
    #[cfg(target_arch = "loongarch64")]
    unsafe fn restore_host_translation_state(&self) {
        csr_write::<{ crate::registers::CSR_EENTRY }>(HOST_GUEST_EXIT_EENTRY.read_current_raw());
        set_ecfg_vs(HOST_ECFG_VS.read_current_raw());
        csr_write::<CSR_PGDL>(HOST_STAGE2_PGDL.read_current_raw());
        csr_write::<CSR_PGDH>(HOST_STAGE2_PGDH.read_current_raw());
        csr_write::<CSR_PWCL>(HOST_STAGE2_PWCL.read_current_raw());
        csr_write::<CSR_PWCH>(HOST_STAGE2_PWCH.read_current_raw());
        csr_write::<CSR_STLBPS>(HOST_STAGE2_STLBPS.read_current_raw());
        csr_write::<CSR_TLBRENTRY>(HOST_STAGE2_TLBRENTRY.read_current_raw());
        core::arch::asm!("invtlb 0x0, $r0, $r0");
    }

    #[cfg(target_arch = "loongarch64")]
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
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
            "st.d $r21, $sp, 96",
            "move $t0, $sp",
            "addi.d $t1, $a0, {host_stack_top_offset}",
            "st.d $t0, $t1, 0",
            "bl {run_guest_asm}",
            "bl {run_guest_panic}",
            host_stack_top_offset = const core::mem::size_of::<crate::context_frame::LoongArchContextFrame>(),
            run_guest_asm = sym _run_guest,
            run_guest_panic = sym Self::run_guest_panic,
        );
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe fn run_guest_panic() -> ! {
        panic!("run_guest_panic: control returned to run_guest");
    }

    #[cfg(target_arch = "loongarch64")]
    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> AxResult<AxVCpuExitReason> {
        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(&mut self.ctx),
            TrapKind::Irq => handle_exception_irq(&mut self.ctx),
        }
    }
}
