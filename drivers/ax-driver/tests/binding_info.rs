use ax_driver::BindingInfo;
#[cfg(feature = "pci")]
use ax_driver::PciIrqRequirement;
#[cfg(feature = "plat-dyn")]
use ax_driver::binding_info_from_acpi_route;
#[cfg(feature = "plat-dyn")]
use ax_driver::binding_info_from_fdt;
#[cfg(feature = "pci")]
use ax_driver::binding_info_from_pci;
#[cfg(feature = "pci")]
use rdrive::probe::pci::{PciAddress, PciInfo};
#[cfg(feature = "plat-dyn")]
use {
    axklib::{
        AxError, AxResult, IrqCpuMask, IrqHandle, Klib, PhysAddr, RawIrqHandler, VirtAddr,
        impl_trait,
    },
    core::time::Duration,
    fdt_edit::{Fdt, Node, Property},
    rdrive::{
        DriverGeneric, Platform, PlatformDevice,
        probe::{
            OnProbeError,
            acpi::{AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger},
        },
        register::{DriverRegister, ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
    },
    std::ptr::NonNull,
    std::sync::Mutex,
};

#[cfg(feature = "plat-dyn")]
static CAPTURED_IRQ: Mutex<Option<Option<usize>>> = Mutex::new(None);
#[cfg(feature = "plat-dyn")]
static SETUP_SPECIFIER: Mutex<Option<Vec<u32>>> = Mutex::new(None);
#[cfg(feature = "plat-dyn")]
static SETUP_ACPI_ROUTE: Mutex<Option<AcpiGsiRoute>> = Mutex::new(None);

#[cfg(feature = "plat-dyn")]
static TEST_INTC_PROBE_KINDS: &[ProbeKind] = &[ProbeKind::Fdt {
    compatibles: &["test,intc"],
    on_probe: register_test_intc,
}];

#[cfg(feature = "plat-dyn")]
static TEST_DEVICE_PROBE_KINDS: &[ProbeKind] = &[ProbeKind::Fdt {
    compatibles: &["test,binding-info"],
    on_probe: capture_binding_info,
}];

#[cfg(feature = "plat-dyn")]
static STATIC_INTC_PROBE_KINDS: &[ProbeKind] = &[ProbeKind::Static {
    on_probe: register_static_test_intc,
}];

#[cfg(feature = "plat-dyn")]
struct KlibImpl;

#[cfg(feature = "plat-dyn")]
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

        fn irq_set_enable(_irq: usize, _enabled: bool) {}

        fn irq_request_shared(
            _irq: usize,
            _handler: RawIrqHandler,
            _data: core::ptr::NonNull<()>,
        ) -> AxResult<IrqHandle> {
            Err(AxError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: usize,
            _cpus: IrqCpuMask,
            _handler: RawIrqHandler,
            _data: core::ptr::NonNull<()>,
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
fn empty_binding_info_has_no_irq() {
    let info = BindingInfo::empty();

    assert_eq!(info.irq_num(), None);
}

#[test]
fn explicit_binding_info_reports_numbered_irq() {
    let info = BindingInfo::with_irq(Some(33));

    assert_eq!(info.irq_num(), Some(33));
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

#[cfg(feature = "plat-dyn")]
#[test]
fn fdt_binding_info_resolves_first_irq_during_probe() {
    *CAPTURED_IRQ.lock().unwrap() = None;
    *SETUP_SPECIFIER.lock().unwrap() = None;

    let fdt_data = Box::leak(Box::new(minimal_irq_fdt().encode()));
    let fdt_addr = NonNull::new(fdt_data.as_ref().as_ptr() as *mut u8).unwrap();

    rdrive::init(Platform::Fdt { addr: fdt_addr }).unwrap();
    rdrive::register_add(DriverRegister {
        name: "binding-info-fdt-test-intc",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::INTC,
        probe_kinds: TEST_INTC_PROBE_KINDS,
    });
    rdrive::register_add(DriverRegister {
        name: "binding-info-fdt-test-device",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: TEST_DEVICE_PROBE_KINDS,
    });
    rdrive::probe_all(true).unwrap();

    assert_eq!(*CAPTURED_IRQ.lock().unwrap(), Some(Some(77)));
    assert_eq!(*SETUP_SPECIFIER.lock().unwrap(), Some(vec![0, 42, 4]));
}

#[cfg(feature = "plat-dyn")]
#[test]
fn acpi_binding_info_sets_up_intc_during_registration() {
    *SETUP_ACPI_ROUTE.lock().unwrap() = None;
    ensure_rdrive_static_intc();

    let info = binding_info_from_acpi_route("\\_SB.TEST", Some(acpi_route())).unwrap();

    assert_eq!(info.irq_num(), Some(88));
    assert_eq!(*SETUP_ACPI_ROUTE.lock().unwrap(), Some(acpi_route()));
}

#[cfg(feature = "plat-dyn")]
struct TestIntc;

#[cfg(feature = "plat-dyn")]
impl DriverGeneric for TestIntc {
    fn name(&self) -> &str {
        "test-intc"
    }
}

#[cfg(feature = "plat-dyn")]
impl rdif_intc::Interface for TestIntc {
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> rdif_intc::IrqId {
        *SETUP_SPECIFIER.lock().unwrap() = Some(irq_prop.to_vec());
        77.into()
    }

    fn setup_irq_by_acpi(&mut self, route: &AcpiGsiRoute) -> rdif_intc::IrqId {
        *SETUP_ACPI_ROUTE.lock().unwrap() = Some(*route);
        88.into()
    }
}

#[cfg(feature = "plat-dyn")]
fn register_test_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_intc::Intc::new(TestIntc));
    Ok(())
}

#[cfg(feature = "plat-dyn")]
fn register_static_test_intc(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    plat_dev.register(rdif_intc::Intc::new(TestIntc));
    Ok(())
}

#[cfg(feature = "plat-dyn")]
fn ensure_rdrive_static_intc() {
    if !rdrive::is_initialized() {
        rdrive::init(Platform::Static).unwrap();
    }
    let has_acpi_intc = rdrive::get_list::<rdif_intc::Intc>()
        .iter()
        .any(|intc| intc.descriptor().name.starts_with("ACPI IOAPIC"));
    if !has_acpi_intc {
        rdrive::register_add(DriverRegister {
            name: "ACPI IOAPIC binding-info-test",
            level: ProbeLevel::PostKernel,
            priority: ProbePriority::INTC,
            probe_kinds: STATIC_INTC_PROBE_KINDS,
        });
        rdrive::probe_all(true).unwrap();
    }
}

#[cfg(feature = "plat-dyn")]
fn capture_binding_info(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    *CAPTURED_IRQ.lock().unwrap() = Some(binding_info_from_fdt(probe.info())?.irq_num());
    Ok(())
}

#[cfg(feature = "plat-dyn")]
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

#[cfg(feature = "plat-dyn")]
fn acpi_route() -> AcpiGsiRoute {
    AcpiGsiRoute {
        gsi: 32,
        vector: 0x50,
        controller_id: 0,
        controller_address: 0xfec0_0000,
        controller_input: 32,
        trigger: AcpiIrqTrigger::Level,
        polarity: AcpiIrqPolarity::ActiveLow,
    }
}

#[cfg(feature = "plat-dyn")]
fn prop_u32s(name: &str, values: &[u32]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(&value.to_be_bytes());
    }
    Property::new(name, data)
}

#[cfg(feature = "plat-dyn")]
fn prop_strs(name: &str, values: &[&str]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }
    Property::new(name, data)
}
