extern crate alloc;

use alloc::format;

use ax_driver_block::ahci::{AhciDriver, AhciHal};
use pcie::CommandRegister;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{EndpointRc, FnOnProbe},
        static_::StaticInfo,
    },
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

pub const DEVICE_NAME: &str = "ahci";

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Static {
            on_probe: probe_static,
        },
        ProbeKind::Pci {
            on_probe: probe_pci as FnOnProbe,
        },
    ],
};

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

fn probe_static(info: StaticInfo, _plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() == DEVICE_NAME {
        log::warn!("AHCI static config is not present; PCI AHCI probe remains enabled");
    }
    Err(OnProbeError::NotMatch)
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
    let Some(driver) = (unsafe { AhciDriver::<AxAhciHal>::try_new(mmio.as_ptr() as usize) }) else {
        return Err(OnProbeError::other("failed to initialize AHCI controller"));
    };
    super::register_block(plat_dev, driver);
    Ok(())
}
