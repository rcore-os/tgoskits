use core::marker::PhantomData;

use super::{
    LoongArchContextFrame,
    exception::{GsprOp, classify_gspr, handle_exception_sync},
    guest_csr::{GuestTimerRegistration, guest_cpucfg_value},
    host::LoongArchHostOps,
    iocsr::{
        LoongArchIocsrStateRef, guest_pending_interrupts, init_guest_iocsr,
        inject_enabled_pending_interrupt, inject_guest_eiointc_vector, update_guest_interrupts,
    },
    registers::{INT_HWI0, INT_IPI},
    trap::{ECODE_GSPR, TrapKind, extract_field, get_badi, get_exception_code, is_host_tlb_refill},
    types::{
        LoongArchAccessFlags, LoongArchGuestPhysAddr, LoongArchHostVirtAddr,
        LoongArchNestedPagingConfig, LoongArchPinnedHost, LoongArchVcpuError, LoongArchVcpuId,
        LoongArchVcpuResult, LoongArchVmExit, LoongArchVmId,
    },
};
use crate::arch::loongarch64::irq::LoongArchRunPort;

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
    pub boot_args: [usize; 3],
    pub boot_stack_top: usize,
    pub firmware_boot: bool,
}

#[repr(C)]
#[derive(Debug)]
pub(crate) struct LoongArchVcpu<H: LoongArchHostOps> {
    machine: ax_cpu::virtualization::Vcpu,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    cpu_id: usize,
    iocsr_state: LoongArchIocsrStateRef,
    guest_timer: GuestTimerRegistration<H>,
    last_badi: usize,
    _host: PhantomData<fn() -> H>,
}

// SAFETY: `LoongArchVcpu` is a crate-private policy wrapper. Its only `!Send`
// field is `ax_cpu::virtualization::Vcpu`, whose `PhantomData<*mut ()>` marker
// exists solely to forbid moving an *active* LVZ binding between CPUs. AxVM
// never moves a bound backend: the only safe bind/unbind callers are this
// crate's `VmArchVcpuOps` adapter and `AxVCpu::BackendBinding`, and
// `AxVCpu::with_exclusive_scope` runs them inside one `PreemptGuard` + CPU pin,
// unloading with `BackendBinding::finish` or aborting in its `Drop` before that
// pin is released. The value therefore crosses threads only while unbound, and
// no safe caller can transfer it while bound. Every other field is plain `Send`
// state once the associated host timer handle is `Send`; the shared
// `Arc<LoongArchIocsrState>` provides its own independent `Send + Sync`.
unsafe impl<H: LoongArchHostOps> Send for LoongArchVcpu<H> where H::TimerHandle: Send {}

impl<H: LoongArchHostOps + 'static> LoongArchVcpu<H> {
    pub(crate) fn new(
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
            machine: {
                let mut machine = ax_cpu::virtualization::Vcpu::default();
                machine.context = ctx;
                machine
            },
            vm_id,
            vcpu_id,
            cpu_id: config.cpu_id,
            iocsr_state: config.iocsr_state,
            guest_timer: GuestTimerRegistration::new(),
            last_badi: 0,
            _host: PhantomData,
        })
    }

    pub(crate) fn set_entry(&mut self, entry: LoongArchGuestPhysAddr) -> LoongArchVcpuResult {
        self.machine.context.sepc = entry.as_usize();
        self.machine.context.gcsr_era = entry.as_usize();
        Ok(())
    }

    pub(crate) fn set_nested_page_table(
        &mut self,
        config: LoongArchNestedPagingConfig,
    ) -> LoongArchVcpuResult {
        self.machine
            .set_root(ax_cpu::PhysAddr::from_usize(config.root_paddr.as_usize()))
            .map_err(|_| LoongArchVcpuError::InvalidInput)
    }

    pub(crate) fn setup(&mut self, config: LoongArchVCpuSetupConfig) -> LoongArchVcpuResult {
        if !config.firmware_boot && config.boot_args != [0; 3] {
            self.machine.context.set_argument(config.boot_args[0]);
            self.machine.context.set_a1(config.boot_args[1]);
            self.machine.context.set_a2(config.boot_args[2]);
            if config.boot_stack_top != 0 {
                self.machine.context.set_gpr(3, config.boot_stack_top);
            }
        }
        self.init_hv();
        Ok(())
    }

    pub(crate) fn prepare_entry(&mut self) {
        if inject_enabled_pending_interrupt(
            &self.iocsr_state,
            &mut self.machine.context,
            self.vcpu_id,
        ) {
            log::trace!(
                "LoongArch guest pending interrupt injected before entry: VM[{}] VCpu[{}] \
                 sepc={:#x}, era={:#x}, estat={:#x}, ecfg={:#x}",
                self.vm_id,
                self.vcpu_id,
                self.machine.context.sepc,
                self.machine.context.gcsr_era,
                self.machine.context.gcsr_estat,
                self.machine.context.gcsr_ectl
            );
        }
        // SAFETY: the surrounding AxVM run interval pins and owns the LVZ bank.
        unsafe {
            ax_cpu::virtualization::set_hwi_pending(
                (self.machine.context.gcsr_estat >> INT_HWI0) as u8,
            )
        };
    }

    pub(crate) fn run_machine(&mut self) -> LoongArchVcpuResult<LoongArchVmExit> {
        let guest_id = self
            .vm_id
            .checked_add(1)
            .and_then(|id| u8::try_from(id).ok())
            .ok_or(LoongArchVcpuError::InvalidInput)?;
        let state_address = ax_cpu::VirtAddr::from_usize(
            0x9000_0000_0000_0000
                | H::virt_to_phys(LoongArchHostVirtAddr::from_usize(
                    (&raw mut self.machine) as usize,
                ))
                .as_usize(),
        );
        // SAFETY: AxVM pins the CPU and masks IRQs across run. Its permanent
        // direct map aliases the retained machine object under both translations.
        let exit = unsafe { self.machine.run(guest_id, state_address) }
            .map_err(|_| LoongArchVcpuError::BadState)?;
        Ok(LoongArchVmExit::Machine(exit))
    }

    pub(crate) fn process_exit(
        &mut self,
        exit: ax_cpu::virtualization::Exit,
        pinned: LoongArchPinnedHost,
    ) -> LoongArchVcpuResult<LoongArchVmExit> {
        self.last_badi = exit.instruction;
        self.vmexit_handler(
            match exit.kind {
                ax_cpu::virtualization::ExitKind::Synchronous => TrapKind::Synchronous,
                ax_cpu::virtualization::ExitKind::Irq => TrapKind::Irq,
            },
            pinned,
        )
    }

    /// Resolves the host CPU-local operands of one LVZ exit while it is pinned.
    ///
    /// `finish_exit` interprets the exit later in plain task context, which may
    /// run on a different host CPU. Only CPUCFG needs a pinned host read;
    /// IOCSR accesses operate entirely on VM-owned state in task context.
    /// Only the operands that instruction needs are captured, and only bounded local
    /// accesses happen: no timer, allocation, device service or sleeping lock is
    /// touched while the backend is bound.
    pub(crate) fn capture_pinned_host(
        &self,
        exit: &ax_cpu::virtualization::Exit,
    ) -> LoongArchPinnedHost {
        if !matches!(exit.kind, ax_cpu::virtualization::ExitKind::Synchronous) {
            return LoongArchPinnedHost::None;
        }
        let ctx = &self.machine.context;
        // Only CPUCFG emulation touches host-local state.
        // Translation refills, hypercalls and page faults are described
        // entirely by the durable guest context.
        if is_host_tlb_refill(ctx) || get_exception_code(ctx) != ECODE_GSPR {
            return LoongArchPinnedHost::None;
        }
        let ins = get_badi(ctx) as u32 as usize;
        match classify_gspr(ctx) {
            Some(GsprOp::Cpucfg) => {
                let rj = extract_field(ins, 5, 5);
                let index = ctx.x[rj];
                LoongArchPinnedHost::Cpucfg {
                    index,
                    value: guest_cpucfg_value(index),
                }
            }
            _ => LoongArchPinnedHost::None,
        }
    }

    pub(crate) fn bind(&mut self) -> LoongArchVcpuResult {
        let original_addresses = ax_cpu::virtualization::entry_addresses();
        let pa = H::virt_to_phys(LoongArchHostVirtAddr::from_usize(
            original_addresses[1].as_usize(),
        ));
        let refill_phys = ax_cpu::PhysAddr::from_usize(pa.as_usize());
        let addresses = original_addresses.map(|address| {
            ax_cpu::VirtAddr::from_usize(
                0x9000_0000_0000_0000
                    | H::virt_to_phys(LoongArchHostVirtAddr::from_usize(address.as_usize()))
                        .as_usize(),
            )
        });
        // SAFETY: the host owner's valid mapping of the original, currently loaded
        // refill symbol yields its physically reachable PA and the matching refill
        // alias in the same entry text. The permanent direct map keeps all three
        // aliases executable; the code and mappings remain valid throughout the
        // binding. AxVM pins the binding and retains this backend until unbind.
        unsafe {
            let entries = ax_cpu::virtualization::EntryAddresses::new(addresses, refill_phys)
                .map_err(|_| LoongArchVcpuError::InvalidInput)?;
            self.machine
                .bind(entries)
                .map_err(|_| LoongArchVcpuError::BadState)
        }
    }

    pub(crate) fn unbind(&mut self) -> LoongArchVcpuResult {
        // SAFETY: AxVM runs unbind on the pinned owner after guest execution ends.
        unsafe { self.machine.unbind() }.map_err(|_| LoongArchVcpuError::BadState)
    }

    pub(crate) fn set_gpr(&mut self, idx: usize, val: usize) {
        self.machine.context.set_gpr(idx, val);
    }

    /// Advances the guest PC past one fully emulated instruction.
    pub(crate) fn advance_guest_pc(&mut self) {
        self.machine.context.advance_guest_pc();
    }

    /// Binds the run-bound publication capability into every lower callback
    /// carrier owned by this backend.
    ///
    /// Task context, called once per run before the vCPU can enter the guest.
    pub(crate) fn set_run_port(&mut self, port: LoongArchRunPort) {
        self.guest_timer.set_run_port(port.clone());
        self.iocsr_state.set_run_port(port);
    }

    pub(crate) fn decode_mmio_fault(
        &mut self,
        fault_addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    ) -> Option<LoongArchVmExit> {
        let gcsr_badi = self.machine.context.gcsr_badi;
        let exit = super::mmio::decode_mmio_fault(
            &self.machine.context,
            self.last_badi,
            fault_addr,
            access_flags,
        )
        .or_else(|| {
            if gcsr_badi == self.last_badi {
                None
            } else {
                super::mmio::decode_mmio_fault(
                    &self.machine.context,
                    gcsr_badi,
                    fault_addr,
                    access_flags,
                )
            }
        });

        if exit.is_none() {
            let (rj, rj_value, normal_addr, ptr_addr) =
                super::mmio::describe_mmio_fault(&self.machine.context, self.last_badi);
            log::debug!(
                "LoongArch MMIO decode failed: addr={:#x}, flags={:?}, sepc={:#x}, badi={:#x}, \
                 gcsr_badi={:#x}, rj={}, rj_value={:#x}, normal_addr={:#x}, ptr_addr={:#x}",
                fault_addr.as_usize(),
                access_flags,
                self.machine.context.sepc,
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

    pub(crate) fn inject_interrupt(&mut self, vector: usize) -> LoongArchVcpuResult {
        if vector <= INT_IPI {
            self.machine.context.gcsr_estat |= 1usize << vector;
        } else if let Some(hwi) =
            inject_guest_eiointc_vector(&self.iocsr_state, self.vm_id, self.vcpu_id, vector)
        {
            self.machine.context.gcsr_estat |= 1usize << hwi;
        } else {
            log::warn!("Ignoring unsupported LoongArch interrupt vector {vector}");
        }
        update_guest_interrupts(&self.iocsr_state, &mut self.machine.context, self.vcpu_id);
        Ok(())
    }

    pub(crate) fn set_return_value(&mut self, val: usize) {
        self.machine.context.set_a0(val);
    }

    pub(crate) fn inject_external_interrupt(
        &mut self,
        vector: usize,
        physical_irq: usize,
    ) -> LoongArchVcpuResult {
        if let Some(hwi) =
            inject_guest_eiointc_vector(&self.iocsr_state, self.vm_id, self.vcpu_id, vector)
        {
            self.machine.context.gcsr_estat |= 1usize << hwi;
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
        // A masked or unrouted source remains pending in the VM IOCSR model.
        // Never reinterpret its source number as a CPU interrupt vector.
        update_guest_interrupts(&self.iocsr_state, &mut self.machine.context, self.vcpu_id);
        Ok(())
    }

    pub(crate) fn inject_eiointc_interrupt(&mut self, vector: usize) -> LoongArchVcpuResult {
        if let Some(hwi) =
            inject_guest_eiointc_vector(&self.iocsr_state, self.vm_id, self.vcpu_id, vector)
        {
            self.machine.context.gcsr_estat |= 1usize << hwi;
            return self.inject_interrupt(hwi);
        }
        log::warn!("Ignoring masked LoongArch EIOINTC vector {vector}");
        update_guest_interrupts(&self.iocsr_state, &mut self.machine.context, self.vcpu_id);
        Ok(())
    }

    pub(crate) fn has_enabled_pending_interrupt(&self) -> bool {
        self.machine.context.gcsr_eentry != 0
            && self.machine.context.gcsr_crmd & CSR_CRMD_IE != 0
            && guest_pending_interrupts(&self.iocsr_state, &self.machine.context, self.vcpu_id)
                & self.machine.context.gcsr_ectl
                & LOCAL_INTERRUPT_MASK
                != 0
    }

    /// Arms the existing run-bound producer while IDLE parks the vCPU task.
    /// The CPU entry owns the hardware timer only while the guest executes.
    pub(crate) fn arm_idle_timer(&mut self) -> LoongArchVcpuResult {
        self.guest_timer.cancel()?;
        let ctx = &mut self.machine.context;
        ctx.update_timer();
        let period = ctx.gcsr_tcfg & !3;
        if ctx.gcsr_tcfg & 1 == 0
            || ctx.gcsr_estat & (1 << 11) != 0
            || (ctx.gcsr_tcfg & 2 == 0 && ctx.gcsr_tval > period)
        {
            return Ok(());
        }
        let deadline =
            H::current_time_nanos().saturating_add(H::ticks_to_nanos(ctx.gcsr_tval as u64));
        self.guest_timer.arm(deadline, self.vm_id, self.vcpu_id)
    }

    /// Cancels the outstanding guest timer in task context.
    ///
    /// This is the explicit shutdown step: it must run on the task-phase owner,
    /// outside any hardware binding, before the backend is retired. A failed
    /// cancellation keeps the host registration owned by this backend (see
    /// `GuestTimerRegistration::cancel`) so the owner can retry from the stop
    /// path instead of losing the handle.
    pub(crate) fn quiet_timer(&mut self) -> LoongArchVcpuResult {
        self.guest_timer.cancel()
    }

    /// Quiesces the guest timer producer while preserving its logical deadline.
    ///
    /// The task-phase owner calls this before parking the vCPU: the host
    /// callback is retired with a completion barrier and its already-published
    /// pending interrupts stay queued, but no guest register or memory is
    /// written. A failed retirement keeps the handle so the owner can retry.
    pub(crate) fn suspend_timer(&mut self) -> LoongArchVcpuResult {
        self.guest_timer.suspend()
    }

    /// Re-arms a suspended guest timer at its preserved absolute deadline.
    ///
    /// The task-phase owner calls this before reopening guest admission. A
    /// deadline that elapsed while suspended is re-armed already due, and a
    /// timer that already fired is left alone because its pending interrupt is
    /// still queued.
    pub(crate) fn resume_timer(&mut self) -> LoongArchVcpuResult {
        self.guest_timer.resume(self.vm_id, self.vcpu_id)
    }

    fn init_hv(&mut self) {
        self.init_vm_context();
        init_guest_iocsr(&self.iocsr_state, self.vcpu_id);
    }

    fn init_vm_context(&mut self) {
        self.init_guest_boot_state();
        self.init_guest_page_table_state();
        self.init_guest_exception_state();

        self.machine.context.gcsr_asid = self.vcpu_id;
        self.machine.context.gcsr_cpuid = self.cpu_id;
    }

    /// Set guest architectural boot state: CRMD (DA mode), PRMD (PIE), EUEN, DMW0-3.
    fn init_guest_boot_state(&mut self) {
        self.machine.context.gcsr_crmd = GUEST_RESET_CRMD_DIRECT;
        self.machine.context.gcsr_prmd = GUEST_BOOT_PRMD;
        self.machine.context.gcsr_euen = 0;
        self.machine.context.gcsr_dmw0 = GUEST_BOOT_DMW;
        self.machine.context.gcsr_dmw1 = 0;
        self.machine.context.gcsr_dmw2 = 0;
        self.machine.context.gcsr_dmw3 = 0;
    }

    /// Zero guest page-table CSRs. Guest starts in DA mode (CRMD.DA=1)
    /// with no active page table; it programs these itself when enabling paging.
    fn init_guest_page_table_state(&mut self) {
        self.machine.context.gcsr_pgdl = 0;
        self.machine.context.gcsr_pgdh = 0;
        self.machine.context.gcsr_pgd = 0;
        self.machine.context.gcsr_pwcl = 0;
        self.machine.context.gcsr_pwch = 0;
        self.machine.context.gcsr_stlbps = 0;
    }

    /// Zero guest exception vectors. The guest programs its own EENTRY
    /// and TLBRENTRY early in boot before any exception can occur.
    fn init_guest_exception_state(&mut self) {
        self.machine.context.gcsr_eentry = 0;
        self.machine.context.gcsr_tlbrentry = 0;
        self.machine.context.gcsr_tlbrprmd = 0;
        self.machine.context.gcsr_tlbrera = 0;
    }

    fn vmexit_handler(
        &mut self,
        exit_reason: TrapKind,
        pinned: LoongArchPinnedHost,
    ) -> LoongArchVcpuResult<LoongArchVmExit> {
        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync::<H>(
                &self.iocsr_state,
                &mut self.machine.context,
                self.vm_id,
                self.vcpu_id,
                &mut self.guest_timer,
                pinned,
            ),
            // The normal host IRQ entry consumes live pending sources when
            // the shared entry guard restores IRQs. Never replay saved ESTAT.
            TrapKind::Irq => Ok(LoongArchVmExit::Nothing),
        }
    }
}

impl<H: LoongArchHostOps> Drop for LoongArchVcpu<H> {
    fn drop(&mut self) {
        if let Err(error) = self.guest_timer.cancel() {
            // Reaching this with a live registration means the task-phase owner
            // never ran `quiet_timer`, and a `Drop` cannot retry. Report the
            // abandoned host timer instead of silently discarding its handle.
            log::error!(
                "abandoning LoongArch guest timer registration while dropping VM[{}] VCpu[{}]; \
                 the task-phase owner must run quiet_timer before retiring the vCPU: {error:?}",
                self.vm_id,
                self.vcpu_id
            );
        }
    }
}

#[cfg(any(test, axtest))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        AxVmError, GuestMemoryPort, MemoryRevision, RunId, VmKey,
        arch::loongarch64::{
            AxvmLoongArchHostOps, AxvmLoongArchVcpu, LoongArch64Arch, LoongArchEntry, LoongArchExit,
        },
        architecture::ops::ArchOps,
        services::{RunServices, RunSignals},
    };

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn iocsr_backend_gate_and_fault_exit() {
        let state = super::super::iocsr::LoongArchIocsrState::new(&[0]).unwrap();
        init_guest_iocsr(&state, 0);
        let backend = LoongArchVcpu::<AxvmLoongArchHostOps>::new(
            1,
            0,
            LoongArchVCpuCreateConfig {
                cpu_id: 0,
                dtb_addr: 0,
                boot_args: [0; 3],
                boot_stack_top: 0,
                firmware_boot: true,
                iocsr_state: state.clone(),
            },
        )
        .unwrap();
        let mut vcpu = AxvmLoongArchVcpu { backend };
        vcpu.backend.machine.context.gcsr_era = 0x1234;
        vcpu.backend.machine.context.gcsr_crmd = CSR_CRMD_IE;
        vcpu.backend.machine.context.gcsr_ectl = LOCAL_INTERRUPT_MASK;
        vcpu.backend.machine.context.gcsr_eentry = 0x8000;
        vcpu.backend.inject_external_interrupt(7, 7).unwrap();
        assert_eq!(
            vcpu.backend.machine.context.gcsr_estat & super::super::trap::HWI_MASK,
            0
        );
        assert!(!vcpu.backend.has_enabled_pending_interrupt());
        let write_ins = (0x19207 << 10) | (5 << 5) | 4;
        vcpu.backend.machine.context.x[5] = 0x420;
        vcpu.backend.machine.context.x[4] = 1 << 48;
        assert!(matches!(
            super::super::iocsr::emulate_iocsr(
                &state,
                &mut vcpu.backend.machine.context,
                write_ins,
                1,
                0,
            )
            .unwrap(),
            LoongArchVmExit::Nothing
        ));
        assert!(vcpu.backend.has_enabled_pending_interrupt());
        vcpu.backend.machine.context.x[5] = 0x4000_0004;
        vcpu.backend.machine.context.x[4] = 0;
        assert!(matches!(
            super::super::iocsr::emulate_iocsr(
                &state,
                &mut vcpu.backend.machine.context,
                (0x19206 << 10) | (5 << 5) | 4,
                1,
                0,
            )
            .unwrap(),
            LoongArchVmExit::Nothing
        ));
        vcpu.backend.machine.context.gcsr_estat |= super::super::trap::HWI_MASK;
        assert!(!vcpu.backend.has_enabled_pending_interrupt());
        vcpu.backend.inject_external_interrupt(7, 7).unwrap();
        assert_eq!(
            vcpu.backend.machine.context.gcsr_estat & super::super::trap::HWI_MASK,
            0
        );

        let run_id = RunId::new(VmKey::new(1, 1), 1);
        let signals = RunSignals::new(run_id, 1).unwrap();
        let plan = axdevice::VmResourcePlanner::new(axdevice::ResourcePools::new())
            .plan(std::vec::Vec::<axdevice::DevicePlanRequest>::new())
            .unwrap();
        let devices = Arc::new(
            axdevice::DeviceRuntimeBuilder::new(axdevice::RuntimeAccessPorts::new())
                .finish(&plan)
                .unwrap(),
        );
        let memory = GuestMemoryPort::new(
            MemoryRevision {
                run: run_id,
                sequence: 1,
            },
            std::vec![],
            || {},
        );
        let services = RunServices::new(run_id, devices, memory, signals.clone());
        let entry = LoongArchEntry {
            pch_pic: Arc::new(axdevice::LoongArchPchPic::new(
                axvm_types::GuestPhysAddr::from(0x1000),
                0x1000,
            )),
            run: LoongArchRunPort::new(signals),
            physical_inputs: [None; crate::arch::loongarch64::irq::LOONGARCH_MAX_IRQ_COUNT],
        };
        // Both a rejected IOCSR access and an invalid GSPR survive finish_exit.
        for ins in [(0x19202 << 10) | (5 << 5) | 4, 0x0648_2004] {
            let ctx = &mut vcpu.backend.machine.context;
            ctx.gcsr_era = 0x1234;
            ctx.host_era = 0x1234;
            ctx.host_tlbrera = 0;
            ctx.host_badi = ins;
            ctx.host_estat = ECODE_GSPR << 16;
            ctx.x[5] = 0x408;
            let machine = ax_cpu::virtualization::Exit {
                kind: ax_cpu::virtualization::ExitKind::Synchronous,
                status: ECODE_GSPR << 16,
                instruction: ins,
                pc: 0x1234,
            };
            let pinned = vcpu.backend.capture_pinned_host(&machine);
            assert_eq!(pinned, LoongArchPinnedHost::None);
            let exit = LoongArch64Arch::finish_exit(
                &mut vcpu,
                &entry,
                LoongArchExit::Machine {
                    exit: machine,
                    host: pinned,
                },
            )
            .unwrap();
            assert_eq!(vcpu.backend.machine.context.gcsr_era, 0x1234);
            let error = match LoongArch64Arch::handle_exit(exit, 0, &services) {
                Err(error) => error,
                Ok(_) => panic!("invalid guest instruction did not produce a fault"),
            };
            let AxVmError::Unsupported { detail, .. } = error else {
                panic!("{error:?}")
            };
            assert!(
                detail.contains("pc=0x1234")
                    && detail.contains(&std::format!("instruction={ins:#x}")),
                "{detail}"
            );
            if ins >> 10 == 0x19202 {
                assert!(
                    detail.contains("address=0x408")
                        && detail.contains("Read")
                        && detail.contains("width=4")
                        && detail.contains("UnknownAddress"),
                    "{detail}"
                );
            }
        }
    }
}
