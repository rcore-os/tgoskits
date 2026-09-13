use core::marker::PhantomData;

use super::{
    LoongArchContextFrame,
    exception::{handle_exception_irq, handle_exception_sync},
    guest_csr::GuestTimerRegistration,
    host::LoongArchHostOps,
    iocsr::{
        LoongArchIocsrStateRef, init_guest_iocsr, inject_enabled_pending_interrupt,
        inject_guest_eiointc_vector,
    },
    registers::{INT_HWI0, INT_IPI},
    trap::{TIMER_BIT, TrapKind},
    types::{
        LoongArchAccessFlags, LoongArchGuestPhysAddr, LoongArchHostVirtAddr,
        LoongArchNestedPagingConfig, LoongArchVcpuError, LoongArchVcpuId, LoongArchVcpuResult,
        LoongArchVmExit, LoongArchVmId,
    },
};

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
pub struct LoongArchVcpu<H: LoongArchHostOps> {
    machine: ax_cpu::virtualization::Vcpu,
    vm_id: LoongArchVmId,
    vcpu_id: LoongArchVcpuId,
    cpu_id: usize,
    iocsr_state: LoongArchIocsrStateRef,
    guest_timer: GuestTimerRegistration<H>,
    last_badi: usize,
    _host: PhantomData<fn() -> H>,
}

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

    pub fn set_entry(&mut self, entry: LoongArchGuestPhysAddr) -> LoongArchVcpuResult {
        self.machine.context.sepc = entry.as_usize();
        self.machine.context.gcsr_era = entry.as_usize();
        Ok(())
    }

    pub fn set_nested_page_table(
        &mut self,
        config: LoongArchNestedPagingConfig,
    ) -> LoongArchVcpuResult {
        self.machine
            .set_root(ax_cpu::PhysAddr::from_usize(config.root_paddr.as_usize()))
            .map_err(|_| LoongArchVcpuError::InvalidInput)
    }

    pub fn setup(&mut self, config: LoongArchVCpuSetupConfig) -> LoongArchVcpuResult {
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

    pub fn prepare_entry(&mut self) {
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

    pub fn run_machine(&mut self) -> LoongArchVcpuResult<LoongArchVmExit> {
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
        if exit.status & TIMER_BIT != 0 {
            // Acknowledge this saved host timer source before the entry IRQ
            // guard opens; deferred policy must not clear a newer timer event.
            ax_cpu::timer::acknowledge_interrupt();
        }
        Ok(LoongArchVmExit::Machine(exit))
    }

    pub fn process_exit(
        &mut self,
        exit: ax_cpu::virtualization::Exit,
    ) -> LoongArchVcpuResult<LoongArchVmExit> {
        self.last_badi = exit.instruction;
        self.vmexit_handler(match exit.kind {
            ax_cpu::virtualization::ExitKind::Synchronous => TrapKind::Synchronous,
            ax_cpu::virtualization::ExitKind::Irq => TrapKind::Irq,
        })
    }

    pub fn bind(&mut self) -> LoongArchVcpuResult {
        let addresses = ax_cpu::virtualization::entry_addresses().map(|address| {
            ax_cpu::VirtAddr::from_usize(
                0x9000_0000_0000_0000
                    | H::virt_to_phys(LoongArchHostVirtAddr::from_usize(address.as_usize()))
                        .as_usize(),
            )
        });
        // SAFETY: the host's permanent direct map provides executable aliases of
        // CPU entry text. AxVM pins the binding and retains this backend until unbind.
        unsafe {
            let entries = ax_cpu::virtualization::EntryAddresses::new(addresses)
                .map_err(|_| LoongArchVcpuError::InvalidInput)?;
            self.machine
                .bind(entries)
                .map_err(|_| LoongArchVcpuError::BadState)
        }
    }

    pub fn unbind(&mut self) -> LoongArchVcpuResult {
        // SAFETY: AxVM runs unbind on the pinned owner after guest execution ends.
        unsafe { self.machine.unbind() }.map_err(|_| LoongArchVcpuError::BadState)
    }

    pub fn set_gpr(&mut self, idx: usize, val: usize) {
        self.machine.context.set_gpr(idx, val);
    }

    pub fn decode_mmio_fault(
        &mut self,
        fault_addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    ) -> Option<LoongArchVmExit> {
        let gcsr_badi = self.machine.context.gcsr_badi;
        let exit = super::mmio::decode_mmio_fault(
            &mut self.machine.context,
            self.last_badi,
            fault_addr,
            access_flags,
        )
        .or_else(|| {
            if gcsr_badi == self.last_badi {
                None
            } else {
                super::mmio::decode_mmio_fault(
                    &mut self.machine.context,
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

    pub fn inject_interrupt(&mut self, vector: usize) -> LoongArchVcpuResult {
        if vector <= INT_IPI {
            self.machine.context.gcsr_estat |= 1usize << vector;
        } else if let Some(hwi) =
            inject_guest_eiointc_vector(&self.iocsr_state, self.vm_id, self.vcpu_id, vector)
        {
            self.machine.context.gcsr_estat |= 1usize << hwi;
        } else {
            log::warn!("Ignoring unsupported LoongArch interrupt vector {vector}");
        }
        Ok(())
    }

    pub fn set_return_value(&mut self, val: usize) {
        self.machine.context.set_a0(val);
    }

    pub fn inject_external_interrupt(
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
        self.inject_interrupt(vector)
    }

    pub fn has_enabled_pending_interrupt(&self) -> bool {
        self.machine.context.gcsr_eentry != 0
            && self.machine.context.gcsr_crmd & CSR_CRMD_IE != 0
            && self.machine.context.gcsr_estat
                & self.machine.context.gcsr_ectl
                & LOCAL_INTERRUPT_MASK
                != 0
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

    fn vmexit_handler(&mut self, exit_reason: TrapKind) -> LoongArchVcpuResult<LoongArchVmExit> {
        match exit_reason {
            TrapKind::Synchronous => handle_exception_sync::<H>(
                &self.iocsr_state,
                &mut self.machine.context,
                self.vm_id,
                self.vcpu_id,
                &mut self.guest_timer,
            ),
            TrapKind::Irq => handle_exception_irq(&mut self.machine.context),
        }
    }
}

impl<H: LoongArchHostOps> Drop for LoongArchVcpu<H> {
    fn drop(&mut self) {
        if let Err(error) = self.guest_timer.cancel() {
            log::warn!(
                "failed to cancel LoongArch guest timer while dropping VM[{}] VCpu[{}]: {error:?}",
                self.vm_id,
                self.vcpu_id
            );
        }
    }
}
