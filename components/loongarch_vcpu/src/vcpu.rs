use ax_errno::AxResult;
#[cfg(not(target_arch = "loongarch64"))]
use ax_errno::ax_err;
use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axvcpu::{AxArchVCpu, AxVCpuExitReason};

use crate::context_frame::{LoongArchContextFrame, LoongArchGuestSystemRegisters};
#[cfg(target_arch = "loongarch64")]
use crate::exception::{TrapKind, handle_exception_irq, handle_exception_sync};

#[cfg(target_arch = "loongarch64")]
unsafe extern "C" {
    fn _run_guest(ctx: *mut LoongArchContextFrame) -> !;
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
    guest_system_regs: LoongArchGuestSystemRegisters,
    cpu_id: usize,
}

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
        ctx.set_argument(config.dtb_addr);

        Ok(Self {
            ctx,
            host_stack_top: 0,
            guest_system_regs: LoongArchGuestSystemRegisters::default(),
            cpu_id: config.cpu_id,
        })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.ctx.sepc = entry.as_usize();
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.guest_system_regs.gpgd = ept_root.as_usize();
        Ok(())
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        self.init_hv(config);
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        #[cfg(target_arch = "loongarch64")]
        {
            let exit_reason = unsafe {
                save_host_sp();
                self.restore_vm_system_regs();
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
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
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
        self.ctx.crmd = 0x5;
        self.ctx.prmd = 0;
        self.ctx.estat = 0;

        if config.passthrough_timer {
            self.guest_system_regs.gtcfg = 0x1;
        }

        if config.passthrough_interrupt {
            log::trace!("LoongArch passthrough interrupt mode enabled");
        }

        self.guest_system_regs.gpgdl = 0;
        self.guest_system_regs.gpgdh = 0;
        self.guest_system_regs.geentry = 0;
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
    unsafe fn restore_vm_system_regs(&mut self) {
        self.guest_system_regs.restore();
    }

    #[cfg(target_arch = "loongarch64")]
    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> AxResult<AxVCpuExitReason> {
        unsafe {
            self.guest_system_regs.store();
        }

        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync(&mut self.ctx),
            TrapKind::Irq => handle_exception_irq(&mut self.ctx),
        }
    }
}
