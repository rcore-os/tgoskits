extern crate alloc;

use alloc::format;

use pcie::CommandRegister;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{EndpointRc, FnOnProbe},
    },
};
use simple_ahci::{AhciDriver as SimpleAhciDriver, Hal as AhciHal};

use super::{SyncBlockOps, register_sync_block};

pub const DEVICE_NAME: &str = "ahci";

crate::model_register!(
    name: "AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

struct AxAhciHal;

impl AhciHal for AxAhciHal {
    fn virt_to_phys(va: usize) -> usize {
        axklib::mem::virt_to_phys(va.into()).as_usize()
    }

    fn current_ms() -> u64 {
        0
    }

    fn flush_dcache() {}
}

fn probe_pci(endpoint: &mut EndpointRc, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
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

    let mmio = axklib::mmio::ioremap_raw(bar.start.into(), bar.count().max(1))
        .map_err(|err| OnProbeError::other(format!("failed to map AHCI BAR: {err:?}")))?;
    let Some(driver) = (unsafe { SimpleAhciDriver::<AxAhciHal>::try_new(mmio.as_ptr() as usize) })
    else {
        return Err(OnProbeError::other("failed to initialize AHCI controller"));
    };
    register_sync_block(plat_dev, AhciBlock(driver));
    Ok(())
}

struct AhciBlock(SimpleAhciDriver<AxAhciHal>);

impl SyncBlockOps for AhciBlock {
    fn name(&self) -> &'static str {
        DEVICE_NAME
    }

    fn num_blocks(&self) -> u64 {
        self.0.capacity()
    }

    fn block_size(&self) -> usize {
        self.0.block_size()
    }

    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> Result<(), rdif_block::BlkError> {
        if !buf.len().is_multiple_of(self.block_size())
            || !(buf.as_ptr() as usize).is_multiple_of(4)
        {
            return Err(rdif_block::BlkError::NotSupported);
        }
        if self.0.read(block_id, buf) {
            Ok(())
        } else {
            Err(rdif_block::BlkError::Other("AHCI read failed"))
        }
    }

    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> Result<(), rdif_block::BlkError> {
        if !buf.len().is_multiple_of(self.block_size())
            || !(buf.as_ptr() as usize).is_multiple_of(4)
        {
            return Err(rdif_block::BlkError::NotSupported);
        }
        if self.0.write(block_id, buf) {
            Ok(())
        } else {
            Err(rdif_block::BlkError::Other("AHCI write failed"))
        }
    }
}
