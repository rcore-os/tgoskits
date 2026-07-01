//! Dummy implementation of platform-related interfaces defined in [`axplat`].

use ax_plat::{
    console::{ConsoleDeviceIdError, ConsoleDeviceIdResult, ConsoleIf},
    impl_plat_interface,
    init::InitIf,
    irq::{HwIrq, IpiTarget, IrqError, IrqId, IrqIf, IrqNumber, IrqSource, TrapVector},
    mem::{MemIf, RawRange},
    power::PowerIf,
    time::TimeIf,
};

struct DummyInit;
struct DummyConsole;
struct DummyMem;
struct DummyTime;
struct DummyPower;
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

    fn claim_runtime_output() {}

    fn irq_num() -> Option<IrqId> {
        None
    }

    fn set_input_irq_enabled(_enabled: bool) {}

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

    fn phys_to_virt(_paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
        va!(0)
    }

    fn virt_to_phys(_vaddr: ax_memory_addr::VirtAddr) -> ax_memory_addr::PhysAddr {
        pa!(0)
    }

    fn kernel_aspace() -> (ax_memory_addr::VirtAddr, usize) {
        (va!(0), 0)
    }
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

    fn irq_num() -> IrqId {
        IrqNumber(0).expect("dummy legacy IRQ exceeds legacy IRQ width")
    }

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
}

#[impl_plat_interface]
impl IrqIf for DummyIrq {
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
