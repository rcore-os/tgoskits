use core::{ptr::NonNull, time::Duration};

use ax_alloc::{UsageKind, global_allocator};
use ax_driver_net::ixgbe::{INTEL_82599, INTEL_VEND, IxgbeHal, IxgbeNic, PhysAddr};
use pcie::CommandRegister;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{EndpointRc, FnOnProbe},
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

const QUEUE_NUM: u16 = 1;
const QUEUE_SIZE: usize = 1024;

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static Intel 82599",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
};

struct IxgbeHalImpl;

unsafe impl IxgbeHal for IxgbeHalImpl {
    fn dma_alloc(size: usize) -> (PhysAddr, NonNull<u8>) {
        let pages = size.div_ceil(0x1000);
        let Ok(vaddr) = global_allocator().alloc_pages(pages, 0x1000, UsageKind::Dma) else {
            return (0, NonNull::dangling());
        };
        let paddr = axklib::mem::virt_to_phys(vaddr.into()).as_usize();
        let ptr = NonNull::new(vaddr as _).expect("DMA allocator returned null");
        (paddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, vaddr: NonNull<u8>, size: usize) -> i32 {
        global_allocator().dealloc_pages(
            vaddr.as_ptr() as usize,
            size.div_ceil(0x1000),
            UsageKind::Dma,
        );
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8> {
        axklib::mmio::ioremap_raw(paddr.into(), size)
            .map(|mmio| mmio.as_nonnull_ptr())
            .expect("failed to map ixgbe MMIO")
    }

    unsafe fn mmio_virt_to_phys(vaddr: NonNull<u8>, _size: usize) -> PhysAddr {
        axklib::mem::virt_to_phys((vaddr.as_ptr() as usize).into()).as_usize()
    }

    fn wait_until(duration: Duration) -> Result<(), &'static str> {
        axklib::time::busy_wait(duration);
        Ok(())
    }
}

fn probe_pci(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if endpoint.vendor_id() != INTEL_VEND || endpoint.device_id() != INTEL_82599 {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("ixgbe BAR0 MMIO region missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd
    });

    let bar_size = bar.end.saturating_sub(bar.start).max(1);
    let mmio = axklib::mmio::ioremap_raw(bar.start.into(), bar_size)
        .map_err(|err| OnProbeError::other(alloc::format!("failed to map ixgbe BAR: {err:?}")))?;
    let driver =
        IxgbeNic::<IxgbeHalImpl, QUEUE_SIZE, QUEUE_NUM>::init(mmio.as_ptr() as usize, bar_size)
            .map_err(|err| OnProbeError::other(alloc::format!("ixgbe init failed: {err:?}")))?;
    super::register_net(plat_dev, driver);
    Ok(())
}
