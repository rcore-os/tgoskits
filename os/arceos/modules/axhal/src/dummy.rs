//! Dummy implementation of platform-related interfaces defined in [`axplat`].

#[cfg(feature = "irq")]
use ax_plat::irq::{HwIrq, IpiTarget, IrqError, IrqId, IrqIf, IrqNumber, IrqSource, TrapVector};
use ax_plat::{
    console::{ConsoleDeviceIdError, ConsoleDeviceIdResult, ConsoleIf},
    impl_plat_interface,
    init::InitIf,
    mem::{DCacheOp, IomapAttrs, IomapDecision, IomapError, MemIf, PhysAddr, RawRange},
    power::PowerIf,
    time::TimeIf,
};

struct DummyInit;
struct DummyConsole;
struct DummyMem;
struct DummyTime;
struct DummyPower;
#[cfg(feature = "irq")]
struct DummyIrq;

#[impl_plat_interface]
impl InitIf for DummyInit {
    fn init_early(_cpu_id: usize, _arg: usize) {}

    #[cfg(feature = "smp")]
    fn init_early_secondary(_cpu_id: usize) {}

    fn init_later(_cpu_id: usize, _arg: usize) {}

    #[cfg(feature = "smp")]
    fn init_later_secondary(_cpu_id: usize) {}
}

#[impl_plat_interface]
impl ConsoleIf for DummyConsole {
    fn write_bytes(_bytes: &[u8]) {
        unimplemented!()
    }

    fn read_bytes(_bytes: &mut [u8]) -> usize {
        unimplemented!()
    }

    fn device_id() -> ConsoleDeviceIdResult {
        Err(ConsoleDeviceIdError::NotSpecified)
    }

    fn physical_mmio_base() -> Option<PhysAddr> {
        None
    }

    fn claim_runtime_output() {}

    fn try_suspend_boot_output() -> bool {
        false
    }

    fn resume_boot_output() {}

    #[cfg(feature = "irq")]
    fn irq_num() -> Option<IrqId> {
        None
    }

    #[cfg(feature = "irq")]
    fn set_input_irq_enabled(_enabled: bool) {}

    #[cfg(feature = "irq")]
    fn handle_irq() -> ax_plat::console::ConsoleIrqEvent {
        ax_plat::console::ConsoleIrqEvent::empty()
    }
}

#[impl_plat_interface]
impl MemIf for DummyMem {
    fn phys_ram_ranges() -> &'static [RawRange] {
        &[]
    }

    fn reserved_phys_ram_ranges() -> &'static [RawRange] {
        &[]
    }

    fn mmio_ranges() -> &'static [RawRange] {
        &[]
    }

    fn prepare_iomap(
        addr: ax_memory_addr::PhysAddr,
        _size: usize,
        _attrs: IomapAttrs,
    ) -> Result<IomapDecision, IomapError> {
        Ok(IomapDecision::UseGeneric(addr))
    }

    fn phys_to_virt(_paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
        va!(0)
    }

    fn virt_to_phys(_vaddr: ax_memory_addr::VirtAddr) -> ax_memory_addr::PhysAddr {
        pa!(0)
    }

    fn kernel_aspace() -> (ax_memory_addr::VirtAddr, usize) {
        (va!(0), 0)
    }

    fn user_aspace_needs_kernel_mappings() -> bool {
        true
    }

    fn dcache_range(_op: DCacheOp, _addr: ax_memory_addr::VirtAddr, _size: usize) {}

    fn dma_coherent_before_make_uncached(_addr: ax_memory_addr::VirtAddr, _size: usize) {}

    fn dma_coherent_before_restore_cached(_addr: ax_memory_addr::VirtAddr, _size: usize) {}

    fn dma_coherent_after_mapping_update() {}
}

#[impl_plat_interface]
impl TimeIf for DummyTime {
    fn current_ticks() -> u64 {
        0
    }

    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks
    }

    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos
    }

    fn epochoffset_nanos() -> u64 {
        0
    }

    #[cfg(feature = "irq")]
    fn irq_num() -> IrqId {
        IrqNumber(0).expect("dummy legacy IRQ exceeds legacy IRQ width")
    }

    #[cfg(feature = "irq")]
    fn set_oneshot_timer(_deadline_ns: u64) {}
}

#[impl_plat_interface]
impl PowerIf for DummyPower {
    #[cfg(feature = "smp")]
    fn cpu_boot(_cpu_id: usize, _stack_top_paddr: usize) {}

    fn system_off() -> ! {
        unimplemented!()
    }

    fn system_reset() -> ! {
        unimplemented!()
    }

    fn cpu_num() -> usize {
        1
    }

    fn cpu_hardware_id(cpu_id: usize) -> Option<usize> {
        (cpu_id == 0).then_some(0)
    }
}

#[cfg(feature = "irq")]
#[impl_plat_interface]
impl IrqIf for DummyIrq {
    fn prepare(_vector: TrapVector) {}

    fn init_boot_irqs(_cpu_id: usize) -> Result<(), IrqError> {
        Ok(())
    }

    #[cfg(feature = "smp")]
    fn init_secondary_boot_irqs(_cpu_id: usize) -> Result<(), IrqError> {
        Ok(())
    }

    fn set_enable(_irq: IrqId, _enabled: bool) -> Result<(), IrqError> {
        Ok(())
    }

    fn set_affinity(
        _irq: IrqId,
        _affinity: ax_plat::irq::IrqAffinity,
    ) -> Result<(), ax_plat::irq::IrqError> {
        Err(ax_plat::irq::IrqError::Unsupported)
    }

    fn handle(_irq: TrapVector) -> Option<IrqId> {
        None
    }

    fn send_ipi(_irq: IrqId, _target: IpiTarget) {}

    fn ipi_irq() -> IrqId {
        IrqId::new(ax_plat::irq::CPU_LOCAL_IRQ_DOMAIN, HwIrq(0))
    }

    fn resolve_source(source: IrqSource) -> Result<IrqId, IrqError> {
        match source {
            IrqSource::ControllerLine { domain, hwirq } => Ok(IrqId::new(domain, hwirq)),
            IrqSource::AcpiGsi(_) | IrqSource::AcpiGsiRoute(_) => Err(IrqError::Unsupported),
        }
    }

    fn resolve_percpu(_hwirq: HwIrq) -> Result<IrqId, IrqError> {
        Err(IrqError::Unsupported)
    }
}
