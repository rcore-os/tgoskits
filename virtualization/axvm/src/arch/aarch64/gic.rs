//! AArch64 GIC host operations for the ArceOS-backed AxVM runtime.

use core::{
    arch::asm,
    sync::atomic::{AtomicU32, Ordering},
};

use arm_gic_driver::{
    IntId,
    v2::{HypervisorInterface, VirtualInterruptConfig, VirtualInterruptState},
    v3::{
        ICH_AP0R0_EL2, ICH_AP0R1_EL2, ICH_AP0R2_EL2, ICH_AP0R3_EL2, ICH_AP1R0_EL2, ICH_AP1R1_EL2,
        ICH_AP1R2_EL2, ICH_AP1R3_EL2, ICH_ELRSR_EL2, ICH_HCR_EL2, ICH_LR_EL2, ICH_VMCR_EL2,
        ICH_VTR_EL2, ReadWriteable, Readable, Writeable, ich_lr_el2_get, ich_lr_el2_write,
    },
};
use ax_lazyinit::LazyInit;
use ax_memory_addr::{PhysAddr, VirtAddr};

use crate::{
    config::VMInterruptMode,
    host::{HostMemory, default_host},
    vcpu::get_current_vcpu,
    vm::{
        PassthroughSpiController,
        passthrough_irq::{
            PhysicalSpiDelivery, PhysicalSpiReclaim, PhysicalSpiRoutePolicy, PhysicalSpiState,
        },
    },
};

const NO_TIMER_IRQ: u32 = u32::MAX;
static HOST_VIRTUAL_TIMER_IRQ: AtomicU32 = AtomicU32::new(NO_TIMER_IRQ);
static VIRTUAL_INTERRUPT_BACKEND: LazyInit<VirtualInterruptBackend> = LazyInit::new();

enum VirtualInterruptBackend {
    GicV2(HypervisorInterface),
    GicV3,
}

fn with_gic<T>(f: impl FnOnce(&mut rdif_intc::Intc) -> T) -> T {
    let mut gic = rdrive::get_one::<rdif_intc::Intc>()
        .expect("failed to get GIC driver")
        .lock()
        .expect("failed to lock GIC driver");
    f(&mut gic)
}

fn detect_virtual_interrupt_backend() -> VirtualInterruptBackend {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            let gich = gic
                .hypervisor_interface()
                .expect("GICv2 has no hypervisor interface");
            info!("Using the pre-published GICv2 hypervisor interface for virtual IRQs");
            return VirtualInterruptBackend::GicV2(gich);
        }
        if gic.typed_mut::<arm_gic_driver::v3::Gic>().is_some() {
            info!("Using per-CPU GICv3 system registers for virtual IRQs");
            return VirtualInterruptBackend::GicV3;
        }
        panic!("no GIC driver found while detecting the virtual interrupt backend");
    })
}

fn virtual_interrupt_backend() -> &'static VirtualInterruptBackend {
    VIRTUAL_INTERRUPT_BACKEND
        .get()
        .expect("virtual interrupt backend was not initialized")
}

/// Hands the physical CPU interface from the host to a passthrough guest.
///
/// The host uses split EOI/deactivate mode while running the hypervisor. A
/// passthrough guest owns the physical CPU interface and must instead start
/// from the architectural combined-EOI mode; otherwise guests that issue only
/// `EOIR` leave their first interrupt active forever. Guest initialization may
/// subsequently select split mode if its driver also issues `DIR`.
pub(crate) fn prepare_passthrough_guest_cpu_interface() {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            gic.cpu_interface().set_eoi_mode(false);
            return;
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            gic.cpu_interface().set_eoi_mode_ns(false);
            return;
        }
        panic!("no GIC driver found while preparing a passthrough guest");
    });
}

/// Restores a reset-state virtual CPU interface before an emulated-IRQ guest
/// initializes its ICC system-register state on this physical CPU.
pub(crate) fn prepare_emulated_guest_cpu_interface() {
    match virtual_interrupt_backend() {
        VirtualInterruptBackend::GicV2(gich) => reset_gic_v2_virtual_interface(gich),
        VirtualInterruptBackend::GicV3 => reset_gic_v3_virtual_interface(),
    }
}

fn reset_gic_v2_virtual_interface(gich: &HypervisorInterface) {
    gich.reset_current_cpu();
    gich.enable();
}

fn clear_gic_v3_active_priorities() {
    let preemption_bits = ICH_VTR_EL2.read(ICH_VTR_EL2::PREBITS) + 1;

    ICH_AP0R0_EL2.set(0);
    ICH_AP1R0_EL2.set(0);
    if preemption_bits >= 6 {
        ICH_AP0R1_EL2.set(0);
        ICH_AP1R1_EL2.set(0);
    }
    if preemption_bits >= 7 {
        ICH_AP0R2_EL2.set(0);
        ICH_AP0R3_EL2.set(0);
        ICH_AP1R2_EL2.set(0);
        ICH_AP1R3_EL2.set(0);
    }
}

fn reset_gic_v3_virtual_interface() {
    ICH_HCR_EL2.set(0);
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;
    for lr in 0..lr_num {
        ich_lr_el2_write(lr, ICH_LR_EL2::STATE::Invalid);
    }
    clear_gic_v3_active_priorities();
    ICH_VMCR_EL2.set(0);
    enable_gic_v3_virtual_interface();

    // SAFETY: `isb` only synchronizes the preceding EL2 system-register
    // writes before this pCPU enters the new guest incarnation.
    unsafe {
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

fn inject_interrupt_gic_v2(gich: &HypervisorInterface, irq: usize) {
    gich.enable();
    gich.set_virtual_interrupt(
        0,
        VirtualInterruptConfig::software(
            // SAFETY: the caller validates the virtual INTID before it reaches the backend.
            unsafe { IntId::raw(irq as u32) },
            None,
            0,
            VirtualInterruptState::Pending,
            false,
            true,
        ),
    );
}

pub(crate) fn inject_interrupt(irq: usize) {
    debug!("Injecting virtual interrupt: {irq}");

    match virtual_interrupt_backend() {
        VirtualInterruptBackend::GicV2(gich) => inject_interrupt_gic_v2(gich, irq),
        VirtualInterruptBackend::GicV3 => inject_interrupt_gic_v3(irq),
    }
}

pub(crate) fn register_guest_virtual_timer_irq_injector() {
    let _ = VIRTUAL_INTERRUPT_BACKEND.get_or_init(detect_virtual_interrupt_backend);
    HOST_VIRTUAL_TIMER_IRQ.store(NO_TIMER_IRQ, Ordering::Release);
    match super::fdt::try_get_host_fdt()
        .map(super::fdt::aarch64_virtual_timer_irq_from_fdt)
        .transpose()
    {
        Ok(Some(Some(irq))) => {
            HOST_VIRTUAL_TIMER_IRQ.store(irq, Ordering::Release);
            info!("AArch64 host virtual-timer PPI {irq} is available for guest forwarding");
        }
        Ok(Some(None)) => warn!("AArch64 host FDT has no enabled architectural timer node"),
        Ok(None) => warn!("AArch64 host FDT is unavailable; virtual timer forwarding is disabled"),
        Err(error) => warn!("Failed to derive AArch64 host virtual-timer PPI: {error}"),
    }
    register_aarch64_hardware_irq_injector(forward_current_guest_timer_irq);
}

fn register_aarch64_hardware_irq_injector(injector: fn(usize) -> bool) {
    ax_crate_interface::call_interface!(
        crate::irq::Aarch64PlatformIrqInjectorIf::register_hardware_irq_injector(injector)
    );
}

fn forward_current_guest_timer_irq(physical_irq: usize) -> bool {
    let Ok(physical_irq) = u32::try_from(physical_irq) else {
        return false;
    };
    if HOST_VIRTUAL_TIMER_IRQ.load(Ordering::Acquire) != physical_irq {
        return false;
    }

    let Some(vcpu) = get_current_vcpu::<super::AxvmArmVcpu>() else {
        #[cfg(feature = "rt-trace")]
        crate::rt_trace::record_unowned_virtual_timer_irq();
        // This PPI can only be a timer bank left by a guest that previously
        // ran on this pCPU. Remove the level source before normal GIC
        // completion; otherwise it immediately retriggers in the host task.
        arm_vcpu::disable_local_guest_timers();
        return false;
    };
    let Some(vm) = crate::get_vm_by_id(vcpu.vm_id()) else {
        return false;
    };
    if vm.interrupt_mode() != VMInterruptMode::Emulated {
        return false;
    }
    let Some(virtual_irq) = vm.with_config(|config| config.aarch64_virtual_timer_irq()) else {
        warn!("VM[{}] has no AArch64 virtual-timer PPI route", vm.id());
        return false;
    };

    #[cfg(feature = "rt-trace")]
    let (host_counter_ticks, cntvoff_el2, counter_frequency_hz) = {
        let host_counter_ticks: u64;
        let cntvoff_el2: u64;
        let counter_frequency_hz: u64;
        // SAFETY: these architectural timer registers are readable at EL2 and
        // the reads have no side effects. CNTVOFF_EL2 is still the current
        // vCPU's offset while handling its physical virtual-timer PPI.
        unsafe {
            core::arch::asm!(
                "mrs {counter_frequency_hz}, CNTFRQ_EL0",
                "mrs {cntvoff_el2}, CNTVOFF_EL2",
                "mrs {host_counter_ticks}, CNTPCT_EL0",
                counter_frequency_hz = out(reg) counter_frequency_hz,
                cntvoff_el2 = out(reg) cntvoff_el2,
                host_counter_ticks = out(reg) host_counter_ticks,
                options(nomem, nostack, preserves_flags),
            );
        }
        (host_counter_ticks, cntvoff_el2, counter_frequency_hz)
    };

    let injected = inject_hardware_interrupt(virtual_irq as usize, physical_irq as usize);
    #[cfg(feature = "rt-trace")]
    {
        let forwarding_finished_ticks: u64;
        // SAFETY: reading CNTPCT_EL0 is side-effect free and available at EL2.
        unsafe {
            core::arch::asm!(
                "mrs {forwarding_finished_ticks}, CNTPCT_EL0",
                forwarding_finished_ticks = out(reg) forwarding_finished_ticks,
                options(nomem, nostack, preserves_flags),
            );
        }
        crate::rt_trace::record_virtual_timer_injection(
            crate::rt_trace::VirtualTimerInjectionRecord {
                sequence: 0,
                vm_id: vm.id(),
                vcpu_id: vcpu.id(),
                pcpu_id: crate::rt_trace::current_pcpu_id(),
                physical_irq,
                virtual_irq,
                host_counter_ticks,
                guest_counter_ticks: host_counter_ticks.wrapping_sub(cntvoff_el2),
                forwarding_ticks: forwarding_finished_ticks.saturating_sub(host_counter_ticks),
                injected,
            },
            counter_frequency_hz,
        );
    }
    injected
}

fn inject_hardware_interrupt(virtual_irq: usize, physical_irq: usize) -> bool {
    match virtual_interrupt_backend() {
        VirtualInterruptBackend::GicV2(gich) => {
            inject_hardware_interrupt_gic_v2(gich, virtual_irq, physical_irq)
        }
        VirtualInterruptBackend::GicV3 => {
            inject_hardware_interrupt_gic_v3(virtual_irq, physical_irq)
        }
    }
}

fn inject_hardware_interrupt_gic_v2(
    gich: &HypervisorInterface,
    virtual_irq: usize,
    physical_irq: usize,
) -> bool {
    let Some(free_lr) =
        (0..gich.get_list_register_count()).find(|&lr| gich.is_list_register_empty(lr))
    else {
        debug!("No free GICv2 LR for hardware virtual IRQ {virtual_irq} from PPI {physical_irq}");
        return false;
    };
    gich.enable();
    gich.set_virtual_interrupt(
        free_lr,
        VirtualInterruptConfig::hardware(
            // SAFETY: both routes come from validated GIC PPI specifiers.
            unsafe { IntId::raw(virtual_irq as u32) },
            physical_irq as u32,
            0,
            VirtualInterruptState::Pending,
            true,
        ),
    );
    true
}

fn inject_hardware_interrupt_gic_v3(virtual_irq: usize, physical_irq: usize) -> bool {
    let Some(free_lr) = find_free_gic_v3_lr() else {
        debug!("No free GICv3 LR for hardware virtual IRQ {virtual_irq} from PPI {physical_irq}");
        return false;
    };
    ich_lr_el2_write(
        free_lr,
        ICH_LR_EL2::VINTID.val(virtual_irq as u64)
            + ICH_LR_EL2::PINTID.val(physical_irq as u64)
            + ICH_LR_EL2::STATE::Pending
            + ICH_LR_EL2::GROUP::SET
            + ICH_LR_EL2::HW::SET,
    );
    enable_gic_v3_virtual_interface();
    true
}

fn inject_interrupt_gic_v3(vector: usize) {
    debug!("Injecting virtual interrupt: vector={vector}");
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;

    for i in 0..lr_num {
        let lr_val = ich_lr_el2_get(i);
        if lr_val.read(ICH_LR_EL2::VINTID) == vector as u64
            && lr_val.matches_any(&[ICH_LR_EL2::STATE::Pending, ICH_LR_EL2::STATE::Active])
        {
            debug!("Virtual interrupt {vector} already pending/active in LR{i}, skipping");
            return;
        }
    }

    let free_lr = find_free_gic_v3_lr()
        .unwrap_or_else(|| panic!("no free list register to inject IRQ {vector}"));

    ich_lr_el2_write(
        free_lr,
        ICH_LR_EL2::VINTID.val(vector as u64) + ICH_LR_EL2::STATE::Pending + ICH_LR_EL2::GROUP::SET,
    );

    enable_gic_v3_virtual_interface();
    debug!("Virtual interrupt {vector} injected successfully in LR{free_lr}");
}

fn find_free_gic_v3_lr() -> Option<usize> {
    let elsr = ICH_ELRSR_EL2.read(ICH_ELRSR_EL2::STATUS);
    let lr_num = ICH_VTR_EL2.read(ICH_VTR_EL2::LISTREGS) as usize + 1;
    (0..lr_num).find(|&lr| (1 << lr) & elsr != 0).or_else(|| {
        (0..lr_num).find(|&lr| ich_lr_el2_get(lr).matches_all(ICH_LR_EL2::STATE::Invalid))
    })
}

fn enable_gic_v3_virtual_interface() {
    if !ICH_HCR_EL2.is_set(ICH_HCR_EL2::EN) {
        warn!("Virtual interrupt interface not enabled, enabling now");
        ICH_HCR_EL2.modify(ICH_HCR_EL2::EN::SET);
    }
}

impl arm_gic_driver::v3::SpiDeliverySpec for PhysicalSpiDelivery {
    fn raw_intid(&self) -> u32 {
        u32::try_from(self.intid).expect("passthrough gate validated its physical SPI INTIDs")
    }

    fn target_affinity(&self) -> arm_gic_driver::v3::Affinity {
        arm_gic_driver::v3::Affinity::from_mpidr(self.target_mpidr as u64)
    }

    fn route_policy(&self) -> arm_gic_driver::v3::SpiRoutePolicy {
        match self.route_policy {
            PhysicalSpiRoutePolicy::Configure => arm_gic_driver::v3::SpiRoutePolicy::Configure,
            PhysicalSpiRoutePolicy::Preserve => arm_gic_driver::v3::SpiRoutePolicy::Preserve,
        }
    }
}

impl arm_gic_driver::v3::SpiReclaimSpec for PhysicalSpiReclaim {
    fn raw_intid(&self) -> u32 {
        u32::try_from(self.intid).expect("passthrough gate validated its physical SPI INTIDs")
    }

    fn record_state(&mut self, state: arm_gic_driver::v3::SpiLineState) {
        self.state = Some(PhysicalSpiState {
            active: state.active,
            pending: state.pending,
        });
    }
}

struct HostGicPassthroughSpiController<'a> {
    gic: &'a mut rdif_intc::Intc,
}

impl PassthroughSpiController for HostGicPassthroughSpiController<'_> {
    fn deliver_spis(&mut self, requests: &[PhysicalSpiDelivery]) -> crate::AxVmResult {
        if let Some(gic) = self.gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic
                .route_enable_and_pend_spis(requests)
                .map_err(|error| crate::AxVmError::interrupt("deliver physical SPIs", error));
        }
        if self.gic.typed_mut::<arm_gic_driver::v2::Gic>().is_some() {
            return Err(crate::AxVmError::unsupported(
                "deliver physical SPIs",
                "GICv2 target routing is not implemented for emulated passthrough devices",
            ));
        }
        Err(crate::AxVmError::resource_unavailable(
            "GIC driver",
            "no supported host GIC is registered",
        ))
    }

    fn reclaim_spis(&mut self, requests: &mut [PhysicalSpiReclaim]) -> crate::AxVmResult {
        if let Some(gic) = self.gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic
                .reclaim_pending_spis(requests)
                .map_err(|error| crate::AxVmError::interrupt("reclaim physical SPIs", error));
        }
        if self.gic.typed_mut::<arm_gic_driver::v2::Gic>().is_some() {
            return Err(crate::AxVmError::unsupported(
                "reclaim physical SPIs",
                "GICv2 pending-state reclamation is not implemented for passthrough devices",
            ));
        }
        Err(crate::AxVmError::resource_unavailable(
            "GIC driver",
            "no supported host GIC is registered",
        ))
    }
}

/// Runs one ownership-gate transition while holding the host GIC lock.
///
/// Passthrough paths always acquire the global GIC lock before the per-VM gate.
/// The caller must release both by returning from this closure before waking a
/// vCPU task.
pub(crate) fn with_passthrough_spi_controller<T>(
    f: impl FnOnce(&mut dyn PassthroughSpiController) -> T,
) -> T {
    with_gic(|gic| f(&mut HostGicPassthroughSpiController { gic }))
}

pub(crate) fn read_gicd_iidr() -> u32 {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.iidr_raw();
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic.iidr_raw();
        }
        panic!("no GIC driver found");
    })
}

pub(crate) fn read_gicd_typer() -> u32 {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return gic.typer_raw();
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return gic.typer_raw();
        }
        panic!("no GIC driver found");
    })
}

pub(crate) fn host_gicd_base() -> PhysAddr {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
            return default_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicd_addr())));
        }
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return default_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicd_addr())));
        }
        panic!("no GIC driver found");
    })
}

pub(crate) fn host_gicr_base() -> PhysAddr {
    with_gic(|gic| {
        if let Some(gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
            return default_host().virt_to_phys(VirtAddr::from(usize::from(gic.gicr_addr())));
        }
        panic!("no GICv3 driver found");
    })
}

pub(crate) fn handle_current_irq() -> Option<usize> {
    // AArch64 ArceOS platform IRQ handlers acknowledge the current IRQ
    // internally. The raw vector argument is ignored by current GIC-backed
    // platforms, so keep the ack/EOI ownership inside the platform handler.
    #[cfg(feature = "rt-trace")]
    ax_std::os::arceos::modules::ax_task::finish_current_idle_wait(
        ax_std::os::arceos::modules::ax_hal::time::current_ticks(),
    );
    ax_std::os::arceos::modules::ax_hal::irq::handle_irq(0).then_some(0)
}

pub(crate) fn fetch_irq() -> usize {
    handle_current_irq().unwrap_or(0)
}
