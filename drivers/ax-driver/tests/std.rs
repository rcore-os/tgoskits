extern crate alloc;

use ax_driver::{
    BindingInfo, BindingIrq, BindingIrqSource, Error, FdtIrqSpec, binding_info_from_acpi_route,
};
use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId as KlibIrqId, Klib,
    KlibError, KlibResult, PhysAddr, VirtAddr, impl_trait,
};
use irq_framework::{
    AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger, HwIrq, IrqDomainId, IrqId,
    IrqSource,
};
use rdrive::{DeviceId, ProbeError, error::DriverError, probe::OnProbeError};

struct TestKlib;

impl_trait! {
    impl Klib for TestKlib {
        fn mem_iomap(_addr: PhysAddr, _size: usize) -> KlibResult<VirtAddr> {
            Err(KlibError::Unsupported)
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_map_dma_coherent_uncached(
            _addr: core::ptr::NonNull<u8>,
            _size: usize,
        ) -> axklib::DmaCoherentMappingOutcome {
            axklib::DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
        }

        fn mem_unmap_dma_coherent(_addr: core::ptr::NonNull<u8>, _size: usize) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_alloc_pages(
            _dma_mask: u64,
            _num_pages: usize,
            _align: usize,
        ) -> KlibResult<core::ptr::NonNull<u8>> {
            Err(KlibError::Unsupported)
        }

        fn dma_dealloc_pages(_addr: core::ptr::NonNull<u8>, _num_pages: usize) {}

        fn time_busy_wait(_dur: core::time::Duration) {}

        fn time_monotonic_nanos() -> u64 {
            0
        }

        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
            false
        }

        fn irq_set_enable(_irq: KlibIrqId, _enabled: bool) -> KlibResult {
            Ok(())
        }

        fn irq_request_shared(
            _irq: KlibIrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_shared_disabled(
            _irq: KlibIrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: KlibIrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_free(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_enable(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_disable(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }
    }
}

fn route() -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: 33,
        vector: 44,
        controller: AcpiGsiController::IoApic,
        controller_id: 1,
        controller_address: 0xfec0_0000,
        controller_input: 5,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    }
}

#[test]
fn ax_driver_binding_info_handles_empty_legacy_and_explicit_irq_ids() {
    let empty = BindingInfo::empty();
    assert!(empty.irq().is_none());
    assert_eq!(empty.irq_num(), None);
    assert!(empty.irq_sources().is_empty());

    let legacy = BindingInfo::with_irq(Some(5)).unwrap();
    assert_eq!(legacy.irq_num(), Some(5));
    assert_eq!(legacy.irq_num_for_source(0), Some(5));
    assert!(legacy.irq_cloned().unwrap().irq_id().is_some());

    let id = IrqId::new(IrqDomainId(7), HwIrq(9));
    let explicit = BindingInfo::with_irq_id(Some(id));
    assert_eq!(explicit.irq_cloned(), Some(BindingIrq::id(id)));
    assert_eq!(explicit.irq_for_source_cloned(0), Some(BindingIrq::id(id)));
    assert_eq!(explicit.irq_num(), None);
}

#[test]
fn ax_driver_binding_info_tracks_multiple_named_irq_sources() {
    let first = BindingIrq::acpi_gsi(33);
    let second = BindingIrq::acpi_gsi_route(route());
    let info = BindingInfo::with_irq_sources([(1, first.clone()), (2, second.clone())]);

    assert_eq!(info.irq_sources().len(), 2);
    assert_eq!(info.irq_for_source(1), Some(&first));
    assert_eq!(info.irq_for_source_cloned(2), Some(second));
    assert_eq!(info.irq(), Some(&first));
    assert_eq!(info.irq_for_source(99), None);
}

#[test]
fn ax_driver_binding_irq_sources_convert_to_framework_sources() {
    let gsi = BindingIrqSource::acpi_gsi(19);
    assert_eq!(gsi.as_irq_source(), Some(IrqSource::AcpiGsi(19)));

    let route_source = BindingIrqSource::acpi_gsi_route(route());
    assert_eq!(
        route_source.as_irq_source(),
        Some(IrqSource::AcpiGsiRoute(route()))
    );

    let controller = DeviceId::from(7);
    let fdt = BindingIrqSource::fdt_interrupt_with_controller(controller, alloc::vec![1, 2, 3]);
    assert_eq!(fdt.as_irq_source(), None);
    assert_eq!(
        BindingIrq::fdt_interrupt_with_controller(controller, alloc::vec![4, 5]),
        BindingIrq::Source(BindingIrqSource::FdtInterrupt(FdtIrqSpec {
            controller,
            cells: alloc::vec![4, 5],
        }))
    );
}

#[test]
fn ax_driver_converts_rdif_intc_acpi_routes_without_losing_metadata() {
    let rdif_route = rdif_intc::AcpiGsiRoute {
        gsi: 77,
        vector: 88,
        controller: rdif_intc::AcpiGsiController::PchPic,
        controller_id: 2,
        controller_address: 0x1000,
        controller_input: 9,
        trigger: rdif_intc::AcpiIrqTrigger::Edge,
        polarity: rdif_intc::AcpiIrqPolarity::ActiveHigh,
    };

    let source = BindingIrqSource::from(rdif_route);
    let converted = match source.as_irq_source().unwrap() {
        IrqSource::AcpiGsiRoute(route) => route,
        _ => panic!("expected route source"),
    };
    assert_eq!(converted.gsi, 77);
    assert_eq!(converted.controller, AcpiGsiController::PchPic);
    assert_eq!(converted.trigger, AcpiIrqTrigger::Edge);
    assert_eq!(converted.polarity, AcpiIrqPolarity::ActiveHigh);

    let info = binding_info_from_acpi_route("mock", Some(rdif_route)).unwrap();
    assert_eq!(info.irq_cloned(), Some(BindingIrq::Source(source)));
}

#[test]
fn ax_driver_error_conversions_preserve_driver_and_probe_categories() {
    let driver_error = Error::from(DriverError::Unsupported("mock"));
    assert!(matches!(driver_error, Error::Driver(_)));
    assert!(alloc::format!("{driver_error}").contains("driver init failed"));

    let probe_error = Error::from(ProbeError::Unsupported("mock-probe"));
    assert!(matches!(probe_error, Error::Probe(_)));
    assert!(alloc::format!("{probe_error}").contains("driver probe failed"));

    let on_probe = Error::from(ProbeError::from(OnProbeError::NotMatch));
    assert!(matches!(on_probe, Error::Probe(_)));
}
