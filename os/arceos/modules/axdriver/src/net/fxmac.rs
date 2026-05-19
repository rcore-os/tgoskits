use ax_driver_net::fxmac::FXmacNic;
use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

pub const DEVICE_NAME: &str = "fxmac";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static FXmac",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_fxmac,
    }],
};

#[ax_crate_interface::impl_interface]
impl ax_driver_net::fxmac::KernelFunc for FXmacKernel {
    fn virt_to_phys(addr: usize) -> usize {
        axklib::mem::virt_to_phys(addr.into()).as_usize()
    }

    fn phys_to_virt(addr: usize) -> usize {
        axklib::mmio::ioremap_raw(addr.into(), 0x1000)
            .map(|mmio| mmio.as_ptr() as usize)
            .unwrap_or(0)
    }

    fn dma_alloc_coherent(pages: usize) -> (usize, usize) {
        use ax_alloc::{UsageKind, global_allocator};

        let Ok(vaddr) = global_allocator().alloc_pages(pages, 0x1000, UsageKind::Dma) else {
            return (0, 0);
        };
        let paddr = axklib::mem::virt_to_phys(vaddr.into()).as_usize();
        (vaddr, paddr)
    }

    fn dma_free_coherent(vaddr: usize, pages: usize) {
        use ax_alloc::{UsageKind, global_allocator};

        global_allocator().dealloc_pages(vaddr, pages, UsageKind::Dma);
    }

    fn dma_request_irq(irq: usize, handler: fn(usize)) {
        let _ = axklib::irq::register(irq, handler);
    }
}

pub struct FXmacKernel;

fn probe_fxmac(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }

    let driver = FXmacNic::init(0)
        .map_err(|err| OnProbeError::other(alloc::format!("FXmac init failed: {err:?}")))?;
    super::register_net(plat_dev, driver);
    Ok(())
}
