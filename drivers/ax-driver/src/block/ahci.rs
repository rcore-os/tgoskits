extern crate alloc;

use alloc::format;
use core::sync::atomic::{Ordering, compiler_fence};

#[cfg(feature = "ahci")]
use pcie::CommandRegister;
use rdrive::probe::OnProbeError;
#[cfg(feature = "ahci")]
use rdrive::probe::pci::{FnOnProbe, ProbePci};
#[cfg(feature = "ls2k1000-ahci")]
use rdrive::register::ProbeFdt;
use simple_ahci::{AhciDriver as SimpleAhciDriver, Hal as AhciHal};

use super::{SyncBlockOps, register_sync_block};

pub const DEVICE_NAME: &str = "ahci";
#[cfg(feature = "ls2k1000-ahci")]
const LS2K1000_DEVICE_NAME: &str = "ls2k1000-ahci";
#[cfg(feature = "ls2k1000-ahci")]
const LS2K1000_DEFAULT_MMIO_SIZE: usize = 0x10_000;
const NANOS_PER_MILLIS: u64 = 1_000_000;

#[cfg(feature = "ahci")]
crate::model_register!(
    name: "AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

#[cfg(feature = "ls2k1000-ahci")]
crate::model_register!(
    name: "LS2K1000 AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,ls-ahci",
            "loongson,ls2k1000-ahci",
            "loongson,2k1000-ahci",
            "generic-ahci",
            "snps,dwc-ahci",
        ],
        on_probe: probe_fdt,
    }],
);

struct AxAhciHal;

impl AhciHal for AxAhciHal {
    fn virt_to_phys(va: usize) -> usize {
        axklib::mem::virt_to_phys(va.into()).as_usize()
    }

    fn current_ms() -> u64 {
        axklib::time::monotonic_nanos() / NANOS_PER_MILLIS
    }

    fn flush_dcache() {
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            core::arch::asm!("dbar 0");
        }

        compiler_fence(Ordering::SeqCst);
    }
}

struct AhciBlock {
    name: &'static str,
    driver: SimpleAhciDriver<AxAhciHal>,
}

impl AhciBlock {
    /// # Safety
    ///
    /// `mmio_base` must point to a valid, exclusively-owned AHCI MMIO register
    /// block that is already mapped with device/uncached semantics.
    unsafe fn try_new(name: &'static str, mmio_base: usize) -> Option<Self> {
        let driver = unsafe { SimpleAhciDriver::<AxAhciHal>::try_new(mmio_base) }?;
        Some(Self { name, driver })
    }
}

impl SyncBlockOps for AhciBlock {
    fn name(&self) -> &'static str {
        self.name
    }

    fn num_blocks(&self) -> u64 {
        self.driver.capacity()
    }

    fn block_size(&self) -> usize {
        self.driver.block_size()
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), rdif_block::BlkError> {
        if !buf.len().is_multiple_of(self.block_size()) {
            return Err(rdif_block::BlkError::InvalidRequest);
        }
        if self.driver.read(block_id, buf) {
            Ok(())
        } else {
            Err(rdif_block::BlkError::Other("AHCI read failed"))
        }
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), rdif_block::BlkError> {
        if !buf.len().is_multiple_of(self.block_size()) {
            return Err(rdif_block::BlkError::InvalidRequest);
        }
        if self.driver.write(block_id, buf) {
            Ok(())
        } else {
            Err(rdif_block::BlkError::Other("AHCI write failed"))
        }
    }
}

#[cfg(feature = "ahci")]
fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint_mut();
    let class = endpoint.revision_and_class();
    if (class.base_class, class.sub_class) != (0x01, 0x06) {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = endpoint.bar_mmio(5).or_else(|| endpoint.bar_mmio(0)) else {
        return Err(OnProbeError::other("AHCI MMIO BAR missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd
    });

    let mmio = crate::mmio::iomap(bar.start, bar.count().max(1))
        .map_err(|err| OnProbeError::other(format!("failed to map AHCI BAR: {err:?}")))?;
    let Some(driver) = (unsafe { AhciBlock::try_new(DEVICE_NAME, mmio.as_ptr() as usize) }) else {
        return Err(OnProbeError::other("failed to initialize AHCI controller"));
    };
    register_sync_block(probe.into_platform_device(), driver);
    Ok(())
}

#[cfg(feature = "ls2k1000-ahci")]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let resource_addr = reg.address as usize;
    let size = reg.size.unwrap_or(LS2K1000_DEFAULT_MMIO_SIZE as u64) as usize;
    let mmio = crate::mmio::iomap(resource_addr, size)?;
    let vaddr = mmio.as_ptr() as usize;

    log::debug!(
        "probing {LS2K1000_DEVICE_NAME}: node={}, reg={resource_addr:#x}, vaddr={vaddr:#x}, \
         size={size:#x}",
        info.node.name(),
    );

    let Some(driver) = (unsafe { AhciBlock::try_new(LS2K1000_DEVICE_NAME, vaddr) }) else {
        return Err(OnProbeError::other(format!(
            "failed to initialize {LS2K1000_DEVICE_NAME} controller"
        )));
    };
    let blocks = driver.num_blocks();
    let block_size = driver.block_size();

    register_sync_block(plat_dev, driver);
    log::info!(
        "registered {LS2K1000_DEVICE_NAME} block device: blocks={blocks}, block_size={block_size}",
    );
    Ok(())
}
