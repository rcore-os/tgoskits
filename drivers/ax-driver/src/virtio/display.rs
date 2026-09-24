//! VirtIO GPU discovery, DMA preparation and IRQ transport gate.

extern crate alloc;

use alloc::{boxed::Box, format, string::ToString, sync::Arc};
use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_sync::PreemptIrqSaveGuard;
use dma_api::{DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDomainId};
use rdif_display::DisplayController;
use rdif_gpu::{BusIdentity, GpuIrqEndpoint, GpuIrqEvent, PciIdentity};
use rdrive::{PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    PhysAddr, Result as VirtIoResult,
    transport::{DeviceStatus, InterruptStatus, Transport},
};
use virtio_gpu::{VirtIoGpu, VirtIoGpuDevice};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::{BindingInfo, display::PlatformDeviceGpu, virtio::VirtIoHalImpl};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO GPU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci { on_probe: probe_pci }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let endpoint = probe.endpoint();
    let class = endpoint.revision_and_class();
    let bus = BusIdentity::Pci(PciIdentity {
        vendor: endpoint.vendor_id(),
        device: endpoint.device_id(),
        subsystem_vendor: endpoint.subsystem_vendor_id(),
        subsystem_device: endpoint.subsystem_id(),
        revision: class.revision_id,
        class: (u32::from(class.base_class) << 16)
            | (u32::from(class.sub_class) << 8)
            | u32::from(class.interface),
    });
    let transport =
        crate::pci::take_virtio_transport_masked(probe.endpoint_mut(), DeviceType::GPU)?;
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Optional)?;
    let coherency = if probe.info().dma_coherent {
        DmaCoherency::Coherent
    } else {
        DmaCoherency::NonCoherent
    };
    register_transport_prepared(
        probe.into_platform_device(),
        transport,
        info,
        coherency,
        bus,
    )
}

pub fn register_transport<T: Transport + 'static>(
    platform: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_prepared(
        platform,
        transport,
        BindingInfo::empty(),
        DmaCoherency::Coherent,
        BusIdentity::Platform {
            name: "virtio-gpu".to_string(),
            compatible: Some("virtio,mmio".to_string()),
        },
    )
}

pub fn register_transport_with_info<T: Transport + 'static>(
    platform: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    register_transport_prepared(
        platform,
        transport,
        info,
        DmaCoherency::Coherent,
        BusIdentity::Platform {
            name: "virtio-gpu".to_string(),
            compatible: Some("virtio,mmio".to_string()),
        },
    )
}

fn register_transport_prepared<T: Transport + 'static>(
    platform: PlatformDevice,
    transport: T,
    info: BindingInfo,
    coherency: DmaCoherency,
    bus: BusIdentity,
) -> Result<(), OnProbeError> {
    let dma = axklib::dma::device(DmaDeviceInfo::new(
        DmaDomainId::Direct,
        coherency,
        DmaConstraints::new(u64::MAX),
    ));
    let (transport, irq_endpoint) = SharedGpuTransport::new(transport);
    let raw = VirtIoGpu::<VirtIoHalImpl, _>::new(transport)
        .map_err(|error| OnProbeError::other(format!("virtio-gpu init: {error:?}")))?;
    let mut identity = VirtIoGpuDevice::<VirtIoHalImpl, SharedGpuTransport<T>>::virtual_identity();
    identity.bus = bus;
    identity.modalias = Some("platform:virtio-gpu".to_string());
    let device = VirtIoGpuDevice::new(raw, dma.info().domain(), identity, Some(irq_endpoint))
        .map_err(|error| OnProbeError::other(format!("virtio-gpu outputs: {error:?}")))?;
    let count = device.output_count();
    let irq = platform.register_gpu_display_with_info(device, dma, info);
    log::info!("registered virtio GPU outputs={count} irq={irq:?}");
    Ok(())
}

struct SharedGpuTransport<T: Transport + 'static> {
    inner: Arc<GpuTransportCell<T>>,
}

impl<T: Transport + 'static> SharedGpuTransport<T> {
    fn new(transport: T) -> (Self, Box<dyn GpuIrqEndpoint>) {
        let inner = Arc::new(GpuTransportCell::new(transport));
        (
            Self {
                inner: Arc::clone(&inner),
            },
            Box::new(GpuIrq { inner }),
        )
    }
}

impl<T: Transport + 'static> Drop for SharedGpuTransport<T> {
    fn drop(&mut self) {
        self.inner.shutting_down.store(true, Ordering::Release);
    }
}

struct GpuTransportCell<T: Transport + 'static> {
    transport: UnsafeCell<T>,
    access_active: AtomicBool,
    ack_deferred: AtomicBool,
    pending_status: AtomicU32,
    shutting_down: AtomicBool,
}

// SAFETY: every mutable transport access is guarded by access_active. Task
// access disables local IRQs; the hard IRQ only try-acquires and never waits.
unsafe impl<T: Transport + 'static> Send for GpuTransportCell<T> {}
// SAFETY: shared references can reach transport only under the same gate.
unsafe impl<T: Transport + 'static> Sync for GpuTransportCell<T> {}

impl<T: Transport + 'static> GpuTransportCell<T> {
    fn new(transport: T) -> Self {
        Self {
            transport: UnsafeCell::new(transport),
            access_active: AtomicBool::new(false),
            ack_deferred: AtomicBool::new(false),
            pending_status: AtomicU32::new(0),
            shutting_down: AtomicBool::new(false),
        }
    }

    fn with_task<R>(&self, operation: impl FnOnce(&mut T) -> R) -> R {
        let _irq_guard = PreemptIrqSaveGuard::new();
        while self
            .access_active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        // SAFETY: the gate excludes all task and IRQ transport borrows.
        let transport = unsafe { &mut *self.transport.get() };
        self.ack_deferred(transport);
        let result = operation(transport);
        self.ack_deferred(transport);
        self.access_active.store(false, Ordering::Release);
        result
    }

    fn ack_deferred(&self, transport: &mut T) {
        if self.ack_deferred.swap(false, Ordering::AcqRel) {
            let status = transport.ack_interrupt();
            self.pending_status
                .fetch_or(status.bits(), Ordering::Release);
        }
    }

    fn handle_irq(&self) -> GpuIrqEvent {
        if self.shutting_down.load(Ordering::Acquire) {
            return GpuIrqEvent {
                handled: false,
                work_pending: false,
            };
        }
        if self
            .access_active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.ack_deferred.store(true, Ordering::Release);
            return GpuIrqEvent {
                handled: false,
                work_pending: true,
            };
        }
        // SAFETY: this hard IRQ owns the non-blocking gate and only performs
        // the transport's bounded ISR read/ack. No queue or resource is touched.
        let status = unsafe { &mut *self.transport.get() }.ack_interrupt();
        self.pending_status
            .fetch_or(status.bits(), Ordering::Release);
        self.access_active.store(false, Ordering::Release);
        GpuIrqEvent {
            handled: !status.is_empty(),
            work_pending: !status.is_empty(),
        }
    }
}

struct GpuIrq<T: Transport + 'static> {
    inner: Arc<GpuTransportCell<T>>,
}

impl<T: Transport + 'static> GpuIrqEndpoint for GpuIrq<T> {
    fn handle_irq(&self) -> GpuIrqEvent {
        self.inner.handle_irq()
    }
}

impl<T: Transport + 'static> Transport for SharedGpuTransport<T> {
    fn device_type(&self) -> virtio_drivers::transport::DeviceType {
        self.inner.with_task(|transport| transport.device_type())
    }

    fn read_device_features(&mut self) -> u64 {
        self.inner.with_task(Transport::read_device_features)
    }

    fn write_driver_features(&mut self, features: u64) {
        self.inner
            .with_task(|transport| transport.write_driver_features(features));
    }

    fn max_queue_size(&mut self, queue: u16) -> u32 {
        self.inner
            .with_task(|transport| transport.max_queue_size(queue))
    }

    fn notify(&mut self, queue: u16) {
        self.inner.with_task(|transport| transport.notify(queue));
    }

    fn get_status(&self) -> DeviceStatus {
        self.inner.with_task(|transport| transport.get_status())
    }

    fn set_status(&mut self, status: DeviceStatus) {
        self.inner
            .with_task(|transport| transport.set_status(status));
    }

    fn set_guest_page_size(&mut self, size: u32) {
        self.inner
            .with_task(|transport| transport.set_guest_page_size(size));
    }

    fn requires_legacy_layout(&self) -> bool {
        self.inner
            .with_task(|transport| transport.requires_legacy_layout())
    }

    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: PhysAddr,
        driver_area: PhysAddr,
        device_area: PhysAddr,
    ) {
        self.inner.with_task(|transport| {
            transport.queue_set(queue, size, descriptors, driver_area, device_area)
        });
    }

    fn queue_unset(&mut self, queue: u16) {
        self.inner
            .with_task(|transport| transport.queue_unset(queue));
    }

    fn queue_used(&mut self, queue: u16) -> bool {
        self.inner
            .with_task(|transport| transport.queue_used(queue))
    }

    fn ack_interrupt(&mut self) -> InterruptStatus {
        let direct = self.inner.with_task(Transport::ack_interrupt);
        let pending = self.inner.pending_status.swap(0, Ordering::AcqRel);
        direct | InterruptStatus::from_bits_retain(pending)
    }

    fn read_config_generation(&self) -> u32 {
        self.inner
            .with_task(|transport| transport.read_config_generation())
    }

    fn read_config_space<U: FromBytes + IntoBytes>(&self, offset: usize) -> VirtIoResult<U> {
        self.inner
            .with_task(|transport| transport.read_config_space(offset))
    }

    fn write_config_space<U: IntoBytes + Immutable>(
        &mut self,
        offset: usize,
        value: U,
    ) -> VirtIoResult<()> {
        self.inner
            .with_task(|transport| transport.write_config_space(offset, value))
    }
}
