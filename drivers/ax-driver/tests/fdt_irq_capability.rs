#![cfg(not(feature = "pci"))]

use core::{ptr::NonNull, time::Duration};
use std::sync::Mutex;

use ax_driver::{binding_info_from_fdt, binding_irq_from_named_fdt_interrupt};
use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId, Klib, KlibError,
    KlibResult, PhysAddr, VirtAddr, impl_trait,
};
use fdt_edit::{Fdt, Node, Property};
use rdrive::{
    Platform,
    probe::{OnProbeError, fdt::ProbeFdt},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

static CAPTURED_ERROR: Mutex<Option<String>> = Mutex::new(None);
static RDRIVE_TEST_LOCK: Mutex<()> = Mutex::new(());

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

        fn time_monotonic_nanos() -> u64 {
            0
        }

        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
            false
        }

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> KlibResult {
            Ok(())
        }

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

static DEVICE_REGISTER: DriverRegister = DriverRegister {
    name: "fdt-irq-capability-device",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,fdt-irq-capability-device"],
        on_probe: validate_binding,
    }],
};

fn validate_binding(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let error = binding_info_from_fdt(probe.info())
        .expect_err("binding must reject a parent without Intc capability");
    *CAPTURED_ERROR.lock().unwrap() = Some(error.to_string());
    Ok(())
}

#[test]
fn fdt_irq_parent_requires_registered_intc_capability() {
    let _guard = RDRIVE_TEST_LOCK.lock().unwrap();
    ensure_rdrive_fdt_initialized();
    rdrive::register_add(DEVICE_REGISTER.clone());
    rdrive::probe_all(true).unwrap();

    assert!(
        CAPTURED_ERROR
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .contains("not an available interrupt-controller provider")
    );
}

#[test]
fn named_fdt_irq_requires_registered_intc_capability() {
    let _guard = RDRIVE_TEST_LOCK.lock().unwrap();
    ensure_rdrive_fdt_initialized();

    let error = rdrive::with_fdt(|fdt| {
        let node = fdt
            .find_compatible(&["test,fdt-irq-capability-device"])
            .pop()
            .unwrap();
        binding_irq_from_named_fdt_interrupt(&node, "main")
    })
    .unwrap()
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("not an available interrupt-controller provider")
    );
}

fn ensure_rdrive_fdt_initialized() {
    if rdrive::is_initialized() {
        return;
    }

    let encoded = fdt_with_unregistered_interrupt_parent().encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .unwrap();
}

fn fdt_with_unregistered_interrupt_parent() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[1]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[1]));
    let intc = fdt.add_node(root, Node::new("interrupt-controller@0"));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_strs("compatible", &["test,unregistered-intc"]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("phandle", &[1]));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(Property::new("interrupt-controller", Vec::new()));
    fdt.node_mut(intc)
        .unwrap()
        .set_property(prop_u32s("#interrupt-cells", &[3]));
    let device = fdt.add_node(root, Node::new("device@0"));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_strs("compatible", &["test,fdt-irq-capability-device"]));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_u32s("interrupt-parent", &[1]));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_u32s("interrupts", &[0, 42, 4]));
    fdt.node_mut(device)
        .unwrap()
        .set_property(prop_strs("interrupt-names", &["main"]));
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
