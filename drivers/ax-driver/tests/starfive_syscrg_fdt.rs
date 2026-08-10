#![cfg(feature = "starfive-soc")]

use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use ax_driver::register::DriverRegister;
use axklib::{
    AxError, AxResult, BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId,
    Klib, PhysAddr, VirtAddr, impl_trait,
};
use fdt_edit::{Fdt, Node, Phandle, Property};
use rdrive::{
    DriverGeneric, Platform,
    probe::{OnProbeError, fdt::ResourcePrepareConfig},
    register::{ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

const SYSCRG_PADDR: usize = 0x1302_0000;
const SYSCRG_MMIO_SIZE: usize = 0x1_0000;
const SYSCRG_PHANDLE: u32 = 3;
const SDIO0_AHB_CLOCK: u32 = 91;
const SDIO0_CARD_CLOCK: u32 = 93;
const SDIO0_AHB_RESET: u32 = 64;
const RESET_STATUS_OFFSET: usize = 0x308;

static SYSCRG_MMIO: AtomicUsize = AtomicUsize::new(0);

unsafe extern "Rust" {
    #[link_name = "__DRIVER_STARFIVE_JH7110_SYSTEM_CLOCK_AND_RESET_CONTROLLER"]
    static SYSCRG_DRIVER: DriverRegister;
}

struct KlibImpl;

impl_trait! {
    impl Klib for KlibImpl {
        fn mem_iomap(addr: PhysAddr, size: usize) -> AxResult<VirtAddr> {
            assert_eq!(addr.as_usize(), SYSCRG_PADDR);
            assert_eq!(size, SYSCRG_MMIO_SIZE);
            let ptr = SYSCRG_MMIO.load(Ordering::SeqCst);
            assert_ne!(ptr, 0, "test SYSCRG MMIO must be initialized before probing");
            Ok(VirtAddr::from_usize(ptr))
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_make_dma_coherent_uncached(
            _addr: VirtAddr,
            _size: usize,
        ) -> axklib::DmaCoherentMappingOutcome {
            axklib::DmaCoherentMappingOutcome::NotStarted(AxError::Unsupported)
        }

        fn mem_restore_dma_cached(_addr: VirtAddr, _size: usize) -> AxResult {
            Err(AxError::Unsupported)
        }

        fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

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

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> AxResult {
            Ok(())
        }

        fn irq_request_shared(_irq: IrqId, _handler: BoxedIrqHandler) -> AxResult<IrqHandle> {
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

struct SyscrgConsumer;

impl DriverGeneric for SyscrgConsumer {
    fn name(&self) -> &str {
        "jh7110-syscrg-consumer"
    }
}

fn probe_consumer(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let clocks = info.clock_lines()?;
    assert_eq!(clocks.len(), 2);
    assert_eq!(clocks[0].name(), Some("ahb"));
    assert_eq!(clocks[0].id().raw(), SDIO0_AHB_CLOCK as usize);
    assert_eq!(clocks[1].name(), Some("ciu"));
    assert_eq!(clocks[1].id().raw(), SDIO0_CARD_CLOCK as usize);

    let resets = info.reset_lines()?;
    assert_eq!(resets.len(), 1);
    assert_eq!(resets[0].name(), Some("ahb"));
    assert_eq!(resets[0].id().raw(), u64::from(SDIO0_AHB_RESET));

    let provider = info
        .phandle_to_device_id(Phandle::from(SYSCRG_PHANDLE))
        .ok_or_else(|| OnProbeError::other("SYSCRG provider has no device id"))?;
    rdrive::get::<rdif_clk::Clk>(provider).map_err(|error| {
        OnProbeError::other(format!("SYSCRG clock capability missing: {error}"))
    })?;
    rdrive::get::<rdif_reset::Reset>(provider).map_err(|error| {
        OnProbeError::other(format!("SYSCRG reset capability missing: {error}"))
    })?;

    let resources =
        info.prepare_resources(ResourcePrepareConfig::default().with_named_clock_rate("ciu"))?;
    assert_eq!(resources.clock_rate("ciu"), Some(0));

    probe.into_platform_device().register(SyscrgConsumer);
    Ok(())
}

static CONSUMER_DRIVER: DriverRegister = DriverRegister {
    name: "JH7110 SYSCRG consumer test",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["test,jh7110-syscrg-consumer"],
        on_probe: probe_consumer,
    }],
};

#[test]
fn combined_syscrg_node_publishes_clock_and_reset_capabilities() {
    let regs = Box::leak(vec![0_u32; SYSCRG_MMIO_SIZE / size_of::<u32>()].into_boxed_slice());
    let reset_status_word = SDIO0_AHB_RESET as usize / u32::BITS as usize;
    regs[RESET_STATUS_OFFSET / size_of::<u32>() + reset_status_word] =
        1 << (SDIO0_AHB_RESET % u32::BITS);
    SYSCRG_MMIO.store(regs.as_mut_ptr() as usize, Ordering::SeqCst);

    let encoded = Box::leak(Box::new(syscrg_consumer_fdt().encode()));
    let addr = NonNull::new(encoded.as_ref().as_ptr() as *mut u8).unwrap();
    rdrive::init(Platform::Fdt { addr }).unwrap();
    rdrive::register_add(unsafe { SYSCRG_DRIVER.clone() });
    rdrive::register_add(CONSUMER_DRIVER.clone());

    rdrive::probe_all(true).expect("combined SYSCRG provider and consumer must probe");

    let provider = rdrive::fdt_phandle_to_device_id(Phandle::from(SYSCRG_PHANDLE)).unwrap();
    assert!(rdrive::get::<rdif_clk::Clk>(provider).is_ok());
    assert!(rdrive::get::<rdif_reset::Reset>(provider).is_ok());
    assert!(rdrive::get_one::<SyscrgConsumer>().is_some());
}

fn syscrg_consumer_fdt() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[2]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[2]));

    // Put the consumer first so the clock probe priority, not FDT order, resolves its provider.
    let consumer = fdt.add_node(root, Node::new("mmc@16010000"));
    for property in [
        prop_strs("compatible", &["test,jh7110-syscrg-consumer"]),
        prop_u32s(
            "clocks",
            &[
                SYSCRG_PHANDLE,
                SDIO0_AHB_CLOCK,
                SYSCRG_PHANDLE,
                SDIO0_CARD_CLOCK,
            ],
        ),
        prop_strs("clock-names", &["ahb", "ciu"]),
        prop_u32s("resets", &[SYSCRG_PHANDLE, SDIO0_AHB_RESET]),
        prop_strs("reset-names", &["ahb"]),
    ] {
        fdt.node_mut(consumer).unwrap().set_property(property);
    }

    let provider = fdt.add_node(root, Node::new("clock-controller@13020000"));
    for property in [
        prop_strs("compatible", &["starfive,jh7110-syscrg"]),
        prop_u32s("phandle", &[SYSCRG_PHANDLE]),
        prop_u32s("#clock-cells", &[1]),
        prop_u32s("#reset-cells", &[1]),
        prop_u32s("reg", &[0, SYSCRG_PADDR as u32, 0, SYSCRG_MMIO_SIZE as u32]),
    ] {
        fdt.node_mut(provider).unwrap().set_property(property);
    }

    fdt
}

fn prop_u32s(name: &str, values: &[u32]) -> Property {
    let mut data = Vec::with_capacity(core::mem::size_of_val(values));
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
