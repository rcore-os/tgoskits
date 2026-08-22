#![cfg(feature = "pci")]

use core::{ptr::NonNull, time::Duration};

use ax_driver::{BindingIrq, BindingIrqSource, DriverGeneric};
use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId, Klib, KlibError,
    KlibResult, PhysAddr, VirtAddr, impl_trait,
};
use fdt_edit::{Fdt, Node, Phandle, Property};
use rdrive::{
    Platform,
    probe::{
        OnProbeError,
        fdt::ProbeFdt,
        pci::{PciAddress, PciInfo, PciIntxRoute},
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

const INTC_DOMAIN: irq_framework::IrqDomainId = irq_framework::IrqDomainId(0);

struct KlibImpl;

impl_trait! {
    impl Klib for KlibImpl {
        fn mem_iomap(_addr: PhysAddr, _size: usize) -> KlibResult<VirtAddr> {
            Err(KlibError::Unsupported)
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_map_dma_coherent_uncached(
            _addr: NonNull<u8>,
            _size: usize,
        ) -> axklib::DmaCoherentMappingOutcome {
            axklib::DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
        }

        fn mem_unmap_dma_coherent(_addr: NonNull<u8>, _size: usize) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}
        fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}
        fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_alloc_pages(
            _dma_mask: u64,
            _num_pages: usize,
            _align: usize,
        ) -> KlibResult<NonNull<u8>> {
            Err(KlibError::Unsupported)
        }

        fn dma_dealloc_pages(_addr: NonNull<u8>, _num_pages: usize) {}
        fn time_busy_wait(_dur: Duration) {}
        fn time_monotonic_nanos() -> u64 { 0 }
        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool { false }
        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> KlibResult { Ok(()) }

        fn irq_request_shared(_irq: IrqId, _handler: BoxedIrqHandler) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_free(_handle: IrqHandle) -> KlibResult { Err(KlibError::Unsupported) }
        fn irq_enable(_handle: IrqHandle) -> KlibResult { Err(KlibError::Unsupported) }
        fn irq_disable(_handle: IrqHandle) -> KlibResult { Err(KlibError::Unsupported) }
    }
}

struct TestIntc;

impl DriverGeneric for TestIntc {
    fn name(&self) -> &str {
        "pci-fdt-test-intc"
    }
}

impl rdif_intc::Interface for TestIntc {
    fn translate_fdt(
        &self,
        _irq_prop: &[u32],
    ) -> Result<rdif_intc::ControllerIrqTranslation, rdif_intc::IrqError> {
        Ok(rdif_intc::ControllerIrqTranslation::new(
            irq_framework::HwIrq(42),
        ))
    }
}

static INTC_REGISTER: DriverRegister = DriverRegister {
    name: "pci-fdt-test-intc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,pci-fdt-intc"],
        on_probe: register_test_intc,
    }],
};

fn register_test_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    probe
        .into_platform_device()
        .register(rdif_intc::Intc::new(INTC_DOMAIN, TestIntc));
    Ok(())
}

#[test]
fn pci_fdt_interrupt_map_requires_and_accepts_registered_intc() {
    let encoded = pci_interrupt_map_fdt().encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .unwrap();

    let error = ax_driver::pci::resolve_intx_binding(endpoint()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not an available interrupt-controller provider")
    );

    rdrive::register_add(INTC_REGISTER.clone());
    rdrive::probe_all(true).unwrap();

    let BindingIrq::Source(BindingIrqSource::FdtInterrupt(spec)) =
        ax_driver::pci::resolve_intx_binding(endpoint())
            .unwrap()
            .unwrap()
    else {
        panic!("expected FDT interrupt-map binding");
    };
    assert_eq!(
        spec.controller,
        rdrive::fdt_phandle_to_device_id(Phandle::from(1)).unwrap()
    );
    assert_eq!(spec.cells, vec![0, 42, 4]);
}

fn endpoint() -> PciInfo {
    PciInfo {
        address: PciAddress::new(0, 0, 2, 0),
        interrupt_pin: 1,
        interrupt_line: 0,
        intx_route: Some(PciIntxRoute {
            root_device: 2,
            root_function: 0,
            root_pin: 1,
        }),
        dma_coherent: false,
    }
}

fn pci_interrupt_map_fdt() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[2]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[2]));

    let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_strs("compatible", &["test,pci-fdt-intc"]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("phandle", &[1]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(Property::new("interrupt-controller", Vec::new()));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[0]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("#interrupt-cells", &[3]));

    let host = fdt.add_node(root, Node::new("pcie@0"));
    fdt.node_mut(host)
        .unwrap()
        .set_property(prop_strs("device_type", &["pci"]));
    fdt.node_mut(host)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[3]));
    fdt.node_mut(host)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[2]));
    fdt.node_mut(host)
        .unwrap()
        .set_property(prop_u32s("#interrupt-cells", &[1]));
    fdt.node_mut(host)
        .unwrap()
        .set_property(prop_u32s("bus-range", &[0, 1]));
    fdt.node_mut(host).unwrap().set_property(prop_u32s(
        "interrupt-map-mask",
        &[0xffff_ffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff],
    ));
    fdt.node_mut(host).unwrap().set_property(prop_u32s(
        "interrupt-map",
        &[
            0x0000_1000,
            0,
            0, // PCI child address: bus 0, device 2, function 0.
            1, // PCI INTA.
            1, // Interrupt controller phandle.
            0,
            42,
            4, // Parent GIC-style interrupt specifier.
        ],
    ));
    fdt
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
