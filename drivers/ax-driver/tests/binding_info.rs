use core::time::Duration;
use std::{ptr::NonNull, sync::Mutex};

#[cfg(feature = "pci")]
use ax_driver::PciIrqRequirement;
#[cfg(feature = "pci")]
use ax_driver::binding_info_from_pci;
use ax_driver::{
    BindingIrq, BindingIrqSource, binding_info_from_acpi_route, binding_info_from_fdt,
    binding_irq_from_named_fdt_interrupt,
};
use ax_kspin_test_runtime as _;
use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId,
    Klib, PhysAddr, VirtAddr, impl_trait,
};
use fdt_edit::{Fdt, Node, Phandle, Property};
#[cfg(feature = "pci")]
use rdrive::probe::pci::{PciAddress, PciInfo};
use rdrive::{
    DriverGeneric, Platform,
    probe::{
        OnProbeError,
        acpi::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger},
    },
    register::{DriverRegister, ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

static CAPTURED_IRQ: Mutex<Option<Option<BindingIrq>>> = Mutex::new(None);
static SETUP_SPECIFIER: Mutex<Option<Vec<u32>>> = Mutex::new(None);
static SETUP_ACPI_ROUTE: Mutex<Option<AcpiGsiRoute>> = Mutex::new(None);
static RDRIVE_TEST_LOCK: Mutex<()> = Mutex::new(());

const TEST_INTC_DOMAIN: irq_framework::IrqDomainId = irq_framework::IrqDomainId(0);

static TEST_INTC_PROBE_KINDS: &[ProbeKind] = &[ProbeKind::Fdt {
    compatibles: &["test,intc"],
    on_probe: register_test_intc,
}];

static TEST_DEVICE_PROBE_KINDS: &[ProbeKind] = &[ProbeKind::Fdt {
    compatibles: &["test,binding-info"],
    on_probe: capture_binding_info,
}];

struct KlibImpl;

impl_trait! {
    impl Klib for KlibImpl {
        fn mem_iomap(_addr: PhysAddr, _size: usize) -> AxResult<VirtAddr> {
            Err(AxError::Unsupported)
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_make_dma_coherent_uncached(_addr: VirtAddr, _size: usize) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn dma_alloc_pages(_dma_mask: u64, _num_pages: usize, _align: usize) -> AxResult<VirtAddr> {
            Err(AxError::Unsupported)
        }

        fn dma_dealloc_pages(_addr: VirtAddr, _num_pages: usize) {}

        fn time_busy_wait(_dur: Duration) {}

        fn time_monotonic_nanos() -> u64 {
            0
        }

        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
            false
        }

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> axklib::AxResult {
            Ok(())
        }

        fn irq_request_shared(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_free(_handle: IrqHandle) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn irq_enable(_handle: IrqHandle) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn irq_disable(_handle: IrqHandle) -> AxResult {
            Err(AxError::Unsupported)
        }
    }
}

#[test]
#[cfg(feature = "pci")]
fn optional_pci_binding_info_can_be_empty() {
    let info = binding_info_from_pci(
        PciInfo {
            address: PciAddress::new(0, 0, 0, 0),
            interrupt_pin: 0,
            interrupt_line: 0,
            intx_route: None,
        },
        PciIrqRequirement::Optional,
    )
    .unwrap();

    assert_eq!(info.irq_num(), None);
}

#[test]
#[cfg(feature = "pci")]
fn required_pci_binding_info_reports_unresolved_irq() {
    let err = binding_info_from_pci(
        PciInfo {
            address: PciAddress::new(0, 0, 0, 0),
            interrupt_pin: 0,
            interrupt_line: 0,
            intx_route: None,
        },
        PciIrqRequirement::Required,
    )
    .unwrap_err();

    assert!(err.to_string().contains("failed to resolve IRQ"));
}

#[test]
fn fdt_binding_info_carries_first_irq_specifier_without_setup() {
    let _guard = RDRIVE_TEST_LOCK.lock().unwrap();
    *CAPTURED_IRQ.lock().unwrap() = None;
    *SETUP_SPECIFIER.lock().unwrap() = None;

    ensure_rdrive_test_intc();
    rdrive::register_add(DriverRegister {
        name: "binding-info-fdt-test-device",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: TEST_DEVICE_PROBE_KINDS,
    });
    rdrive::probe_all(true).unwrap();

    let captured = CAPTURED_IRQ.lock().unwrap().clone();
    let Some(Some(BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)))) = captured else {
        panic!("expected captured FDT interrupt binding");
    };
    assert_eq!(spec.cells, vec![0, 42, 4]);
    let controller = rdrive::fdt_phandle_to_device_id(Phandle::from(1)).unwrap();
    assert_eq!(spec.controller, controller);
    assert_eq!(*SETUP_SPECIFIER.lock().unwrap(), None);
}

#[test]
fn named_fdt_interrupt_binding_selects_matching_specifier() {
    let _guard = RDRIVE_TEST_LOCK.lock().unwrap();
    *CAPTURED_IRQ.lock().unwrap() = None;
    *SETUP_SPECIFIER.lock().unwrap() = None;

    ensure_rdrive_fdt_initialized();

    let irq = rdrive::with_fdt(|fdt| {
        let node = fdt.find_compatible(&["test,binding-info"]).pop().unwrap();
        binding_irq_from_named_fdt_interrupt(&node, "backup")
    })
    .unwrap()
    .unwrap()
    .unwrap();

    let BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)) = irq else {
        panic!("expected named FDT interrupt binding");
    };
    let controller = rdrive::fdt_phandle_to_device_id(Phandle::from(1)).unwrap();
    assert_eq!(spec.controller, controller);
    assert_eq!(spec.cells, vec![0, 43, 4]);
    assert_eq!(*SETUP_SPECIFIER.lock().unwrap(), None);
}

#[test]
fn acpi_binding_info_preserves_route_without_setup() {
    let _guard = RDRIVE_TEST_LOCK.lock().unwrap();
    *SETUP_ACPI_ROUTE.lock().unwrap() = None;
    ensure_rdrive_test_intc();

    let info = binding_info_from_acpi_route("\\_SB.TEST", Some(acpi_route())).unwrap();

    assert_eq!(info.irq_num(), None);
    assert_eq!(*SETUP_ACPI_ROUTE.lock().unwrap(), None);
    assert_eq!(
        info.irq(),
        Some(&BindingIrq::Source(BindingIrqSource::AcpiGsiRoute(
            irq_framework::AcpiGsiRoute {
                gsi: acpi_route().gsi,
                vector: acpi_route().vector,
                controller: irq_framework::AcpiGsiController::IoApic,
                controller_id: acpi_route().controller_id,
                controller_address: acpi_route().controller_address,
                controller_input: acpi_route().controller_input,
                trigger: irq_framework::AcpiIrqTrigger::Level,
                polarity: irq_framework::AcpiIrqPolarity::ActiveLow,
            }
        )))
    );
}

struct TestIntc;

impl DriverGeneric for TestIntc {
    fn name(&self) -> &str {
        "test-intc"
    }
}

impl rdif_intc::Interface for TestIntc {
    fn supports_acpi_gsi(&self, route: &AcpiGsiRoute) -> bool {
        *route == acpi_route()
    }

    fn translate_fdt(
        &self,
        irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        *SETUP_SPECIFIER.lock().unwrap() = Some(irq_prop.to_vec());
        Ok(rdif_intc::ControllerIrqTranslation::new(
            irq_framework::HwIrq(77),
        ))
    }

    fn translate_acpi(
        &self,
        _route: &AcpiGsiRoute,
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        Ok(rdif_intc::ControllerIrqTranslation::new(
            irq_framework::HwIrq(88),
        ))
    }

    fn configure_acpi(
        &mut self,
        _translation: &rdif_intc::IrqTranslation,
        route: &AcpiGsiRoute,
    ) -> Result<(), rdif_intc::IrqError> {
        *SETUP_ACPI_ROUTE.lock().unwrap() = Some(*route);
        Ok(())
    }
}

fn register_test_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_intc::Intc::new(TEST_INTC_DOMAIN, TestIntc));
    Ok(())
}

fn ensure_rdrive_fdt_initialized() {
    if !rdrive::is_initialized() {
        let fdt_data = Box::leak(Box::new(minimal_irq_fdt().encode()));
        let fdt_addr = NonNull::new(fdt_data.as_ref().as_ptr() as *mut u8).unwrap();
        rdrive::init(Platform::Fdt { addr: fdt_addr }).unwrap();
    }
}

fn ensure_rdrive_test_intc() {
    ensure_rdrive_fdt_initialized();
    let controller = rdrive::fdt_phandle_to_device_id(Phandle::from(1)).unwrap();
    if rdrive::get::<rdif_intc::Intc>(controller).is_ok() {
        return;
    }
    rdrive::register_add(DriverRegister {
        name: "binding-info-fdt-test-intc",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::INTC,
        probe_kinds: TEST_INTC_PROBE_KINDS,
    });
    rdrive::probe_all(true).unwrap();
}

fn capture_binding_info(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    *CAPTURED_IRQ.lock().unwrap() = Some(binding_info_from_fdt(probe.info())?.irq_cloned());
    Ok(())
}

fn minimal_irq_fdt() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[1]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[1]));

    let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
    fdt.node_mut(intc).unwrap().set_property(prop_strs(
        "compatible",
        &["test,intc", "test,intc-fallback"],
    ));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("phandle", &[1]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(Property::new("interrupt-controller", Vec::new()));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("#interrupt-cells", &[3]));

    let dev = fdt.add_node(root, Node::new("device@0"));
    fdt.node_mut(dev).unwrap().set_property(prop_strs(
        "compatible",
        &["test,binding-info", "test,binding-info-fallback"],
    ));
    fdt.node_mut(dev)
        .unwrap()
        .set_property(prop_u32s("interrupt-parent", &[1]));
    fdt.node_mut(dev)
        .unwrap()
        .set_property(prop_u32s("interrupts", &[0, 42, 4, 0, 43, 4]));
    fdt.node_mut(dev)
        .unwrap()
        .set_property(prop_strs("interrupt-names", &["main", "backup"]));

    fdt
}

fn acpi_route() -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: 32,
        vector: 0x50,
        controller: AcpiGsiController::IoApic,
        controller_id: 0,
        controller_address: 0xfec0_0000,
        controller_input: 32,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    }
}

fn prop_u32s(name: &str, values: &[u32]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(&value.to_be_bytes());
    }
    Property::new(name, data)
}

fn prop_strs(name: &str, values: &[&str]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }
    Property::new(name, data)
}
