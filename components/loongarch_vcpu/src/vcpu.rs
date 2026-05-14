use ax_errno::AxResult;
#[cfg(not(target_arch = "loongarch64"))]
use ax_errno::ax_err;
#[cfg(target_arch = "loongarch64")]
use ax_memory_addr::VirtAddr;
#[cfg(target_arch = "loongarch64")]
use ax_page_table_multiarch::loongarch64::LA64MetaData;
use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axvcpu::{AxArchVCpu, AxVCpuExitReason};
#[cfg(target_arch = "loongarch64")]
use axvisor_api::memory;
use axvisor_api::vmm::{VCpuId, VMId};
#[cfg(target_arch = "loongarch64")]
use loongArch64::register::prmd;

#[cfg(target_arch = "loongarch64")]
use crate::exception::{TrapKind, handle_exception_irq, handle_exception_sync};
use crate::{
    context_frame::LoongArchContextFrame,
    registers::{
        CSR_PGDH, CSR_PGDL, CSR_PWCH, CSR_PWCL, CSR_STLBPS, CSR_TLBRENTRY, GCSR_ASID, GCSR_BADI,
        GCSR_BADV, GCSR_CNTC, GCSR_CPUID, GCSR_CRMD, GCSR_DMW0, GCSR_DMW1, GCSR_DMW2, GCSR_DMW3,
        GCSR_ECTL, GCSR_EENTRY, GCSR_ERA, GCSR_ESTAT, GCSR_EUEN, GCSR_LLBCTL, GCSR_MISC, GCSR_PGD,
        GCSR_PGDH, GCSR_PGDL, GCSR_PRCFG1, GCSR_PRCFG2, GCSR_PRCFG3, GCSR_PRMD, GCSR_PWCH,
        GCSR_PWCL, GCSR_RAVCFG, GCSR_SAVE0, GCSR_SAVE1, GCSR_SAVE2, GCSR_SAVE3, GCSR_SAVE4,
        GCSR_SAVE5, GCSR_SAVE6, GCSR_SAVE7, GCSR_SAVE8, GCSR_SAVE9, GCSR_SAVE10, GCSR_SAVE11,
        GCSR_SAVE12, GCSR_SAVE13, GCSR_SAVE14, GCSR_SAVE15, GCSR_STLBPS, GCSR_TCFG, GCSR_TICLR,
        GCSR_TID, GCSR_TLBEHI, GCSR_TLBELO0, GCSR_TLBELO1, GCSR_TLBIDX, GCSR_TLBRBADV,
        GCSR_TLBREHI, GCSR_TLBRELO0, GCSR_TLBRELO1, GCSR_TLBRENTRY, GCSR_TLBRERA, GCSR_TLBRPRMD,
        GCSR_TLBRSAVE, GCSR_TVAL, INT_TIMER, csr_read, csr_write, gcfg_set_gpm_num, gcfg_set_matc,
        gcfg_set_toci, gcfg_set_toe, gcfg_set_tohu, gcfg_set_top, gcfg_set_topi, gcfg_set_toti,
        gcsr_read, gintc_set_hwip, gstat_set_gid, gstat_set_pgm, gtlbc_set_tgid,
        gtlbc_set_use_tgid, set_ecfg_line_enabled, set_ecfg_vs,
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
#[cfg(target_arch = "loongarch64")]
const GUEST_EXCEPTION_ENTRY_OFFSET: usize = 0x1000;
#[cfg(target_arch = "loongarch64")]
const GUEST_TLB_REFILL_ENTRY_OFFSET: usize = 0x2000;

#[derive(Clone, Debug, Default)]
pub struct LoongArchVCpuCreateConfig {
    pub cpu_id: usize,
    pub dtb_addr: usize,
}

#[derive(Clone, Debug, Default)]
pub struct LoongArchVCpuSetupConfig {
    pub passthrough_interrupt: bool,
    pub passthrough_timer: bool,
    pub kernel_load_gpa: usize,
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
        let entry = entry.as_usize();
        self.ctx.sepc = entry;
        self.ctx.gcsr_era = entry;
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
            self.activate_stage2();
            // Our guest-exit vector table uses 0x80-byte spacing between entries,
            // i.e. 32 instructions, so CSR.ECFG.VS must be 5.
            set_ecfg_vs(5);
            // Follow hvisor's minimal LVZ guest-entry configuration:
            // each VM gets its own non-zero GID/TGID, guest mode is enabled
            // just before entering the guest, and guest-sensitive traps are
            // relaxed for the current bring-up stage.
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
            // hvisor forwards all 8 hardware interrupt lines before guest entry
            // through GINTC.HWIP, not HWIS.
            gintc_set_hwip(0xff);
            set_ecfg_line_enabled(INT_TIMER, false);
            // Guest entry happens through `ertn`, so PRMD.PIE must be enabled
            // to restore interrupt-enable state on return to guest context.
            prmd::set_pie(true);
            log::debug!(
                "LoongArch guest bind: host_ecfg={:#x}, host_eentry={:#x}, stage2_root={:#x}",
                csr_read::<{ crate::registers::CSR_ECFG }>(),
                csr_read::<{ crate::registers::CSR_EENTRY }>(),
                self.stage2_root.as_usize()
            );
        }
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            set_ecfg_line_enabled(INT_TIMER, true);
            self.restore_host_stage2();
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
        self.load_reset_guest_csrs();
        #[cfg(target_arch = "loongarch64")]
        self.init_guest_boot_csrs(config.kernel_load_gpa);

        if config.passthrough_timer {
            self.ctx.gcsr_tcfg = 0x1;
        }

        if config.passthrough_interrupt {
            log::trace!("LoongArch passthrough interrupt mode enabled");
        }

        self.ctx.gcsr_asid = self.vcpu_id;
        self.ctx.gcsr_cpuid = self.cpu_id;
        self.ctx.gcsr_era = self.ctx.sepc;

        #[cfg(target_arch = "loongarch64")]
        log::info!(
            "LoongArch guest reset state: crmd={:#x}, prmd={:#x}, dmw0={:#x}, dmw1={:#x}, \
             pgdl={:#x}, pgdh={:#x}",
            self.ctx.gcsr_crmd,
            self.ctx.gcsr_prmd,
            self.ctx.gcsr_dmw0,
            self.ctx.gcsr_dmw1,
            self.ctx.gcsr_pgdl,
            self.ctx.gcsr_pgdh
        );
    }

    #[cfg(target_arch = "loongarch64")]
    fn init_guest_boot_csrs(&mut self, kernel_load_gpa: usize) {
        // QEMU-LVZ currently exposes an all-zero guest CSR bank before the first
        // entry, so we must construct the boot architectural state explicitly.
        // ArceOS LoongArch boot starts at a physical entry point, expects direct
        // address translation (DA=1, PG=0), then programs DMW0 and later enables
        // page-table-based translation by itself.
        let guest_boot_base_gva = GUEST_BOOT_DMW & !((1usize << GUEST_DMW_DA_BITS) - 1);

        self.ctx.gcsr_crmd = GUEST_RESET_CRMD_DIRECT;
        self.ctx.gcsr_prmd = GUEST_BOOT_PRMD;
        self.ctx.gcsr_euen = 0;
        self.ctx.gcsr_dmw0 = GUEST_BOOT_DMW;
        self.ctx.gcsr_dmw1 = 0;
        self.ctx.gcsr_dmw2 = 0;
        self.ctx.gcsr_dmw3 = 0;
        self.ctx.gcsr_pgdl = 0;
        self.ctx.gcsr_pgdh = 0;
        self.ctx.gcsr_pgd = 0;
        self.ctx.gcsr_pwcl = 0;
        self.ctx.gcsr_pwch = 0;
        self.ctx.gcsr_stlbps = 0;
        self.ctx.gcsr_eentry = guest_boot_base_gva + kernel_load_gpa + GUEST_EXCEPTION_ENTRY_OFFSET;
        self.ctx.gcsr_tlbrentry = kernel_load_gpa + GUEST_TLB_REFILL_ENTRY_OFFSET;
        self.ctx.gcsr_tlbrprmd = 0;
        self.ctx.gcsr_tlbrera = 0;
    }

    #[cfg(target_arch = "loongarch64")]
    fn load_reset_guest_csrs(&mut self) {
        self.ctx.gcsr_crmd = unsafe { gcsr_read::<GCSR_CRMD>() };
        self.ctx.gcsr_prmd = unsafe { gcsr_read::<GCSR_PRMD>() };
        self.ctx.gcsr_euen = unsafe { gcsr_read::<GCSR_EUEN>() };
        self.ctx.gcsr_misc = unsafe { gcsr_read::<GCSR_MISC>() };
        self.ctx.gcsr_ectl = unsafe { gcsr_read::<GCSR_ECTL>() };
        self.ctx.gcsr_estat = unsafe { gcsr_read::<GCSR_ESTAT>() };
        self.ctx.gcsr_era = unsafe { gcsr_read::<GCSR_ERA>() };
        self.ctx.gcsr_badv = unsafe { gcsr_read::<GCSR_BADV>() };
        self.ctx.gcsr_badi = unsafe { gcsr_read::<GCSR_BADI>() };
        self.ctx.gcsr_eentry = unsafe { gcsr_read::<GCSR_EENTRY>() };
        self.ctx.gcsr_tlbidx = unsafe { gcsr_read::<GCSR_TLBIDX>() };
        self.ctx.gcsr_tlbehi = unsafe { gcsr_read::<GCSR_TLBEHI>() };
        self.ctx.gcsr_tlbelo0 = unsafe { gcsr_read::<GCSR_TLBELO0>() };
        self.ctx.gcsr_tlbelo1 = unsafe { gcsr_read::<GCSR_TLBELO1>() };
        self.ctx.gcsr_asid = unsafe { gcsr_read::<GCSR_ASID>() };
        self.ctx.gcsr_pgdl = unsafe { gcsr_read::<GCSR_PGDL>() };
        self.ctx.gcsr_pgdh = unsafe { gcsr_read::<GCSR_PGDH>() };
        self.ctx.gcsr_pgd = unsafe { gcsr_read::<GCSR_PGD>() };
        self.ctx.gcsr_pwcl = unsafe { gcsr_read::<GCSR_PWCL>() };
        self.ctx.gcsr_pwch = unsafe { gcsr_read::<GCSR_PWCH>() };
        self.ctx.gcsr_stlbps = unsafe { gcsr_read::<GCSR_STLBPS>() };
        self.ctx.gcsr_ravcfg = unsafe { gcsr_read::<GCSR_RAVCFG>() };
        self.ctx.gcsr_cpuid = unsafe { gcsr_read::<GCSR_CPUID>() };
        self.ctx.gcsr_prcfg1 = unsafe { gcsr_read::<GCSR_PRCFG1>() };
        self.ctx.gcsr_prcfg2 = unsafe { gcsr_read::<GCSR_PRCFG2>() };
        self.ctx.gcsr_prcfg3 = unsafe { gcsr_read::<GCSR_PRCFG3>() };
        self.ctx.gcsr_save0 = unsafe { gcsr_read::<GCSR_SAVE0>() };
        self.ctx.gcsr_save1 = unsafe { gcsr_read::<GCSR_SAVE1>() };
        self.ctx.gcsr_save2 = unsafe { gcsr_read::<GCSR_SAVE2>() };
        self.ctx.gcsr_save3 = unsafe { gcsr_read::<GCSR_SAVE3>() };
        self.ctx.gcsr_save4 = unsafe { gcsr_read::<GCSR_SAVE4>() };
        self.ctx.gcsr_save5 = unsafe { gcsr_read::<GCSR_SAVE5>() };
        self.ctx.gcsr_save6 = unsafe { gcsr_read::<GCSR_SAVE6>() };
        self.ctx.gcsr_save7 = unsafe { gcsr_read::<GCSR_SAVE7>() };
        self.ctx.gcsr_save8 = unsafe { gcsr_read::<GCSR_SAVE8>() };
        self.ctx.gcsr_save9 = unsafe { gcsr_read::<GCSR_SAVE9>() };
        self.ctx.gcsr_save10 = unsafe { gcsr_read::<GCSR_SAVE10>() };
        self.ctx.gcsr_save11 = unsafe { gcsr_read::<GCSR_SAVE11>() };
        self.ctx.gcsr_save12 = unsafe { gcsr_read::<GCSR_SAVE12>() };
        self.ctx.gcsr_save13 = unsafe { gcsr_read::<GCSR_SAVE13>() };
        self.ctx.gcsr_save14 = unsafe { gcsr_read::<GCSR_SAVE14>() };
        self.ctx.gcsr_save15 = unsafe { gcsr_read::<GCSR_SAVE15>() };
        self.ctx.gcsr_tid = unsafe { gcsr_read::<GCSR_TID>() };
        self.ctx.gcsr_tcfg = unsafe { gcsr_read::<GCSR_TCFG>() };
        self.ctx.gcsr_tval = unsafe { gcsr_read::<GCSR_TVAL>() };
        self.ctx.gcsr_cntc = unsafe { gcsr_read::<GCSR_CNTC>() };
        self.ctx.gcsr_ticlr = unsafe { gcsr_read::<GCSR_TICLR>() };
        self.ctx.gcsr_llbctl = unsafe { gcsr_read::<GCSR_LLBCTL>() };
        self.ctx.gcsr_tlbrentry = unsafe { gcsr_read::<GCSR_TLBRENTRY>() };
        self.ctx.gcsr_tlbrbadv = unsafe { gcsr_read::<GCSR_TLBRBADV>() };
        self.ctx.gcsr_tlbrera = unsafe { gcsr_read::<GCSR_TLBRERA>() };
        self.ctx.gcsr_tlbrsave = unsafe { gcsr_read::<GCSR_TLBRSAVE>() };
        self.ctx.gcsr_tlbrelo0 = unsafe { gcsr_read::<GCSR_TLBRELO0>() };
        self.ctx.gcsr_tlbrelo1 = unsafe { gcsr_read::<GCSR_TLBRELO1>() };
        self.ctx.gcsr_tlbrehi = unsafe { gcsr_read::<GCSR_TLBREHI>() };
        self.ctx.gcsr_tlbrprmd = unsafe { gcsr_read::<GCSR_TLBRPRMD>() };
        self.ctx.gcsr_dmw0 = unsafe { gcsr_read::<GCSR_DMW0>() };
        self.ctx.gcsr_dmw1 = unsafe { gcsr_read::<GCSR_DMW1>() };
        self.ctx.gcsr_dmw2 = unsafe { gcsr_read::<GCSR_DMW2>() };
        self.ctx.gcsr_dmw3 = unsafe { gcsr_read::<GCSR_DMW3>() };
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe fn activate_stage2(&self) {
        HOST_GUEST_EXIT_EENTRY.write_current_raw(csr_read::<{ crate::registers::CSR_EENTRY }>());
        HOST_STAGE2_PGDL.write_current_raw(csr_read::<CSR_PGDL>());
        HOST_STAGE2_PGDH.write_current_raw(csr_read::<CSR_PGDH>());
        HOST_STAGE2_PWCL.write_current_raw(csr_read::<CSR_PWCL>());
        HOST_STAGE2_PWCH.write_current_raw(csr_read::<CSR_PWCH>());
        HOST_STAGE2_STLBPS.write_current_raw(csr_read::<CSR_STLBPS>());
        HOST_STAGE2_TLBRENTRY.write_current_raw(csr_read::<CSR_TLBRENTRY>());

        let root = self.stage2_root.as_usize();
        unsafe extern "C" {
            fn handle_tlb_refill();
        }
        let tlbrentry_vaddr = VirtAddr::from_ptr_of(handle_tlb_refill as *const ());
        let tlbrentry_paddr = memory::virt_to_phys(tlbrentry_vaddr).as_usize();
        let guest_exit_eentry = core::ptr::addr_of!(_exception_vectors) as usize;

        csr_write::<CSR_PWCL>(LA64MetaData::PWCL_VALUE as usize);
        csr_write::<CSR_PWCH>(LA64MetaData::PWCH_VALUE as usize);
        csr_write::<CSR_STLBPS>(12);
        csr_write::<CSR_PGDL>(root);
        csr_write::<CSR_PGDH>(root);
        csr_write::<CSR_TLBRENTRY>(tlbrentry_paddr);
        // Follow hvisor's split: host CSR.EENTRY handles guest-exit traps,
        // while guest GCSR.EENTRY remains guest-owned architectural state.
        csr_write::<{ crate::registers::CSR_EENTRY }>(guest_exit_eentry);
        core::arch::asm!("invtlb 0x0, $r0, $r0");
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe fn restore_host_stage2(&self) {
        csr_write::<{ crate::registers::CSR_EENTRY }>(HOST_GUEST_EXIT_EENTRY.read_current_raw());
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
