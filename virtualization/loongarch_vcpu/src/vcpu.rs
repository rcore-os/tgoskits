use ax_errno::AxResult;
#[cfg(not(target_arch = "loongarch64"))]
use ax_errno::ax_err;
#[cfg(target_arch = "loongarch64")]
use ax_memory_addr::VirtAddr;
use axvm_types::{
    GuestPhysAddr, HostPhysAddr, MappingFlags, NestedPagingConfig, VCpuId, VMId, VmArchVcpuOps,
    VmExit,
};
#[cfg(target_arch = "loongarch64")]
use loongArch64::register::prmd;

#[cfg(target_arch = "loongarch64")]
use crate::exception::{TrapKind, current_badi, handle_exception_irq, handle_exception_sync};
use crate::{
    context_frame::LoongArchContextFrame,
    exception::LoongArchIocsrStateRef,
    host,
    registers::{
        CSR_ASID, CSR_CRMD, CSR_ECFG, CSR_KSAVE_KSP, CSR_PGDH, CSR_PGDL, CSR_PRMD, CSR_PWCH,
        CSR_PWCL, CSR_STLBPS, CSR_TLBRENTRY, INT_HWI0, INT_HWI7, INT_IPI, INT_TIMER, csr_read,
        csr_write, gcfg_set_gpm_num, gcfg_set_matc, gcfg_set_toci, gcfg_set_toe, gcfg_set_tohu,
        gcfg_set_top, gcfg_set_topi, gcfg_set_toti, get_ecfg_vs, gintc_set_hwi_passthrough,
        gstat_set_gid, gstat_set_pgm, gtlbc_set_tgid, gtlbc_set_use_tgid, set_ecfg_line_enabled,
        set_ecfg_vs,
    },
};

#[cfg(target_arch = "loongarch64")]
unsafe extern "C" {
    fn _run_guest(ctx: *mut LoongArchContextFrame) -> !;
    fn _guest_tlb_refill_vector();
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
    iocsr_state: LoongArchIocsrStateRef,
    guest_timer_token: Option<usize>,
    last_badi: usize,
    entry_logged: bool,
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
static HOST_STAGE2_ASID: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_CRMD: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_PRMD: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_KSAVE_KSP: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_GUEST_EXIT_EENTRY: usize = 0;
#[cfg(target_arch = "loongarch64")]
#[ax_percpu::def_percpu]
static HOST_ECFG_VS: usize = 0;

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
const CSR_CRMD_IE: usize = 1 << 2;
#[cfg(target_arch = "loongarch64")]
const LOCAL_INTERRUPT_MASK: usize = (1 << (INT_IPI + 1)) - 1;
#[cfg(target_arch = "loongarch64")]
const HOST_DMW_CACHED_BASE: usize = 0x9000_0000_0000_0000;

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

impl VmArchVcpuOps for LoongArchVCpu {
    type CreateConfig = LoongArchVCpuCreateConfig;
    type SetupConfig = LoongArchVCpuSetupConfig;

    fn new(_vm_id: usize, _vcpu_id: usize, config: Self::CreateConfig) -> AxResult<Self> {
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
            stage2_root: HostPhysAddr::from_usize(0),
            vm_id: _vm_id,
            vcpu_id: _vcpu_id,
            cpu_id: config.cpu_id,
            iocsr_state: config.iocsr_state,
            guest_timer_token: None,
            last_badi: 0,
            entry_logged: false,
        })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.ctx.sepc = entry.as_usize();
        self.ctx.gcsr_era = entry.as_usize();
        Ok(())
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxResult {
        self.stage2_root = config.root_paddr;
        Ok(())
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
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

    fn run(&mut self) -> AxResult<VmExit> {
        #[cfg(target_arch = "loongarch64")]
        {
            unsafe {
                self.enable_guest_mode();
            }
            if crate::exception::inject_enabled_pending_interrupt(
                &self.iocsr_state,
                &mut self.ctx,
                self.vcpu_id,
            ) {
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
            let exit_reason = unsafe {
                save_host_sp();
                self.run_guest()
            };

            log::trace!(
                "LoongArch guest exit raw: VM[{}] VCpu[{}] reason={}, sepc={:#x}, gera={:#x}",
                self.vm_id,
                self.vcpu_id,
                exit_reason,
                self.ctx.sepc,
                self.ctx.gcsr_era
            );
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

    fn decode_mmio_fault(
        &mut self,
        fault_addr: GuestPhysAddr,
        access_flags: MappingFlags,
    ) -> Option<VmExit> {
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

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        if (INT_HWI0..=INT_HWI7).contains(&vector) {
            crate::registers::inject_interrupt(vector);
        } else if vector <= INT_IPI {
            self.ctx.gcsr_estat |= 1usize << vector;
        } else if let Some(hwi) = crate::exception::inject_guest_eiointc_vector(
            &self.iocsr_state,
            self.vm_id,
            self.vcpu_id,
            vector,
        ) {
            crate::registers::inject_interrupt(hwi);
        } else {
            log::warn!("Ignoring unsupported LoongArch interrupt vector {vector}");
        }
        Ok(())
    }

    fn set_return_value(&mut self, val: usize) {
        self.ctx.set_a0(val);
    }
}

impl LoongArchVCpu {
    #[cfg(target_arch = "loongarch64")]
    pub fn inject_external_interrupt(&mut self, vector: usize, physical_irq: usize) -> AxResult {
        if let Some(hwi) = crate::exception::inject_guest_eiointc_vector(
            &self.iocsr_state,
            self.vm_id,
            self.vcpu_id,
            vector,
        ) {
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

    #[cfg(target_arch = "loongarch64")]
    fn host_dmw_alias(vaddr: VirtAddr) -> usize {
        HOST_DMW_CACHED_BASE | host::virt_to_phys(vaddr).as_usize()
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn has_enabled_pending_interrupt(&self) -> bool {
        self.ctx.gcsr_eentry != 0
            && self.ctx.gcsr_crmd & CSR_CRMD_IE != 0
            && self.ctx.gcsr_estat & self.ctx.gcsr_ectl & LOCAL_INTERRUPT_MASK != 0
    }

    #[cfg(target_arch = "loongarch64")]
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

        let nanos = host::ticks_to_nanos(ticks as u64);
        core::time::Duration::from_nanos(nanos).clamp(MIN_WAIT, MAX_WAIT)
    }

    fn init_hv(&mut self, config: LoongArchVCpuSetupConfig) {
        self.init_vm_context(config);
        #[cfg(target_arch = "loongarch64")]
        crate::exception::init_guest_iocsr(&self.iocsr_state, self.vcpu_id);
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            gintc_set_hwi_passthrough(0);
        }
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

        let _ = config.passthrough_timer;
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
    unsafe fn save_host_translation_state(&mut self) {
        HOST_CRMD.write_current_raw(csr_read::<CSR_CRMD>());
        HOST_PRMD.write_current_raw(csr_read::<CSR_PRMD>());
        HOST_KSAVE_KSP.write_current_raw(csr_read::<CSR_KSAVE_KSP>());
        HOST_GUEST_EXIT_EENTRY.write_current_raw(csr_read::<{ crate::registers::CSR_EENTRY }>());
        HOST_ECFG_VS.write_current_raw(get_ecfg_vs());
        HOST_STAGE2_PGDL.write_current_raw(csr_read::<CSR_PGDL>());
        HOST_STAGE2_PGDH.write_current_raw(csr_read::<CSR_PGDH>());
        HOST_STAGE2_PWCL.write_current_raw(csr_read::<CSR_PWCL>());
        HOST_STAGE2_PWCH.write_current_raw(csr_read::<CSR_PWCH>());
        HOST_STAGE2_STLBPS.write_current_raw(csr_read::<CSR_STLBPS>());
        HOST_STAGE2_TLBRENTRY.write_current_raw(csr_read::<CSR_TLBRENTRY>());
        HOST_STAGE2_ASID.write_current_raw(csr_read::<CSR_ASID>());
        self.ctx.host_pgdl = HOST_STAGE2_PGDL.read_current_raw();
        self.ctx.host_pgdh = HOST_STAGE2_PGDH.read_current_raw();
        self.ctx.host_pwcl = HOST_STAGE2_PWCL.read_current_raw();
        self.ctx.host_pwch = HOST_STAGE2_PWCH.read_current_raw();
        self.ctx.host_stlbps = HOST_STAGE2_STLBPS.read_current_raw();
        self.ctx.host_tlbrentry = HOST_STAGE2_TLBRENTRY.read_current_raw();
        self.ctx.host_asid = HOST_STAGE2_ASID.read_current_raw();
        self.ctx.host_eentry = HOST_GUEST_EXIT_EENTRY.read_current_raw();
        self.ctx.host_ecfg = csr_read::<CSR_ECFG>();
        self.ctx.guest_tlbrentry =
            Self::host_dmw_alias(VirtAddr::from_ptr_of(_guest_tlb_refill_vector as *const ()));
        self.ctx.guest_eentry = Self::host_dmw_alias(VirtAddr::from_ptr_of(core::ptr::addr_of!(
            _exception_vectors
        )));
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
        csr_write::<CSR_ASID>(HOST_STAGE2_ASID.read_current_raw());
        csr_write::<CSR_KSAVE_KSP>(HOST_KSAVE_KSP.read_current_raw());
        csr_write::<CSR_PRMD>(HOST_PRMD.read_current_raw());
        csr_write::<CSR_CRMD>(HOST_CRMD.read_current_raw());
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

    #[cfg(target_arch = "loongarch64")]
    extern "C" fn host_dmw_alias_for_asm(ptr: usize) -> usize {
        Self::host_dmw_alias(VirtAddr::from(ptr))
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe fn run_guest_panic() -> ! {
        panic!("run_guest_panic: control returned to run_guest");
    }

    #[cfg(target_arch = "loongarch64")]
    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> AxResult<VmExit> {
        self.last_badi = if self.ctx.host_badi != 0 {
            self.ctx.host_badi
        } else {
            current_badi()
        };

        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(
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
