//! PCI VirtIO Sound adapter.

use alloc::format;

use dma_api::DeviceDma;
use pcie::{CommandRegister, PciCapability};
use rdrive::{PlatformDevice, probe::OnProbeError};
use virtio_drivers::transport::{DeviceType, Transport};
use virtio_sound::VirtioSound;

use crate::virtio::VirtIoHalImpl;

const DISCOVERY_TIMEOUT_NS: u64 = 30_000_000_000;
/// MSI Message Control bit 16 enables legacy MSI delivery.
const PCI_MSI_ENABLE: u32 = 1 << 16;

crate::model_register!(
    name: "VirtIO Sound",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    if !crate::pci::virtio_transport_matches(probe.endpoint(), DeviceType::Sound) {
        return Err(OnProbeError::NotMatch);
    }
    if !info.dma_coherent {
        return Err(OnProbeError::Unsupported(
            "VirtIO Sound requires coherent PCI DMA",
        ));
    }
    if info.iommu.is_some() {
        return Err(OnProbeError::Unsupported(
            "VirtIO Sound does not support translated DMA",
        ));
    }

    {
        let endpoint = probe.endpoint_mut();
        endpoint.update_command(|mut command| {
            command.insert(CommandRegister::INTERRUPT_DISABLE);
            command
        });
        mask_interrupt_sources(endpoint)?;
    }
    // INTx and MSI sources are masked before the transport transfers PCI
    // configuration ownership. Polling service owns completions, so no IRQ
    // action is needed.
    let transport =
        crate::pci::take_virtio_transport_masked(probe.endpoint_mut(), DeviceType::Sound)?;
    // VirtIoHalImpl shares queue memory as physical addresses, so PCM buffers
    // must remain in the same direct DMA domain.
    let dma = crate::pci::device_dma(probe.info(), u64::MAX)?;
    let now_ns = axklib::time::monotonic_nanos();
    let discovery_deadline_ns = now_ns
        .checked_add(DISCOVERY_TIMEOUT_NS)
        .ok_or_else(|| OnProbeError::other("VirtIO Sound discovery deadline overflowed"))?;

    register_transport(
        probe.into_platform_device(),
        transport,
        dma,
        now_ns,
        discovery_deadline_ns,
    )?;
    log::info!("registered VirtIO Sound PCM card at {}", info.address);
    Ok(())
}

fn mask_interrupt_sources(endpoint: &mut pcie::Endpoint) -> Result<(), OnProbeError> {
    for capability in endpoint.capabilities() {
        match capability {
            PciCapability::Msi(_) => {
                let offset = capability.address().offset;
                let control = endpoint.read(offset);
                endpoint.write(offset, control & !PCI_MSI_ENABLE);
            }
            PciCapability::MsiX(_) => {
                endpoint.set_msix_function_mask(true).map_err(|error| {
                    OnProbeError::other(format!("failed to mask VirtIO Sound MSI-X: {error:?}"))
                })?;
                endpoint.set_msix_enabled(false).map_err(|error| {
                    OnProbeError::other(format!("failed to disable VirtIO Sound MSI-X: {error:?}"))
                })?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn register_transport<T: Transport + Send + 'static>(
    platform: PlatformDevice,
    transport: T,
    dma: DeviceDma,
    now_ns: u64,
    discovery_deadline_ns: u64,
) -> Result<(), OnProbeError> {
    // SAFETY: `take_virtio_transport_masked` removed the endpoint from the
    // probe handle and moved its transport into `transport`; this adapter
    // installs no IRQ or passthrough action, and INTx/MSI/MSI-X are masked. The
    // card keeps the transport until a confirmed status-zero reset or
    // destruction.
    // `dma_coherent` was required and IOMMU rejected, so `VirtIoHalImpl`'s
    // physical DMA pages and `DeviceDma`'s direct backend use the same
    // coherent address domain. The core gives queue descriptors their declared
    // read/write direction and retains every in-flight DMA owner.
    let card = unsafe {
        VirtioSound::<VirtIoHalImpl, T>::new(transport, dma, now_ns, discovery_deadline_ns)
    };
    let mut sound =
        card.map_err(|error| OnProbeError::other(format!("VirtIO Sound init failed: {error:?}")))?;
    // Completions are polled by the PCM service task; queue kicks after adding
    // available descriptors remain enabled inside the portable core.
    sound.set_used_notifications(false);
    crate::audio::register_pcm_card(platform, sound);
    Ok(())
}
