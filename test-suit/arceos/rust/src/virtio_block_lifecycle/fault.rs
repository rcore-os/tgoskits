//! Negotiation and reset fault injection around a real QEMU MMIO transport.

use core::{
    cell::Cell,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use rdrive::{probe::OnProbeError, register::ProbeFdt};
use virtio_drivers::{
    PhysAddr,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport, mmio::MmioTransport},
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

const REJECTED_BLOCKS: u64 = 32768;
static REJECTION_CHECKED: AtomicBool = AtomicBool::new(false);

pub fn assert_rejection_checked() {
    assert!(
        REJECTION_CHECKED.load(Ordering::Acquire),
        "feature rejection must be exercised"
    );
}

pub const DELAYED_BLOCKS: u64 = 8192;
pub const STALLED_BLOCKS: u64 = 16384;

ax_driver::model_register!(
    name: "VirtIO block reset fault injection",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::EARLY_DEVICE,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["virtio,mmio"],
        on_probe: probe,
    }],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    let (kind, transport) = ax_driver::virtio::probe_fdt_mmio_device(&info)?;
    if kind != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    let capacity: u32 = transport
        .read_config_space(0)
        .map_err(|_| OnProbeError::other("read test device capacity"))?;
    if !matches!(
        u64::from(capacity),
        2048 | DELAYED_BLOCKS | STALLED_BLOCKS | REJECTED_BLOCKS
    ) {
        return Err(OnProbeError::NotMatch);
    }
    let fault = match u64::from(capacity) {
        2048 => Fault::None,
        REJECTED_BLOCKS => Fault::RejectFeatures,
        STALLED_BLOCKS => Fault::StallReset,
        _ => Fault::DelayReset(Cell::new(3)),
    };
    let reject_features = matches!(fault, Fault::RejectFeatures);
    let result = ax_driver::virtio::block::register_fdt_transport(
        &info,
        platform,
        ResetFaultTransport {
            inner: transport,
            fault,
            initialized: false,
            reset_requested: false,
            queue_memory: None,
        },
    );
    if reject_features {
        assert!(
            result.is_err(),
            "device rejecting FEATURES_OK must not be registered"
        );
        REJECTION_CHECKED.store(true, Ordering::Release);
        Ok(())
    } else {
        result
    }
}

enum Fault {
    None,
    RejectFeatures,
    StallReset,
    DelayReset(Cell<usize>),
}

struct ResetFaultTransport {
    inner: MmioTransport<'static>,
    fault: Fault,
    initialized: bool,
    reset_requested: bool,
    queue_memory: Option<(PhysAddr, PhysAddr, u32)>,
}

impl Transport for ResetFaultTransport {
    fn device_type(&self) -> DeviceType {
        self.inner.device_type()
    }
    fn read_device_features(&mut self) -> u64 {
        self.inner.read_device_features()
    }
    fn write_driver_features(&mut self, features: u64) {
        self.inner.write_driver_features(features);
    }
    fn max_queue_size(&mut self, queue: u16) -> u32 {
        self.inner.max_queue_size(queue)
    }
    fn notify(&mut self, queue: u16) {
        assert!(!self.reset_requested, "doorbell after reset started");
        if matches!(self.fault, Fault::None) {
            let (descriptors, driver, size) = self.queue_memory.expect("configured queue");
            let ram = |address: PhysAddr| {
                ax_std::os::arceos::modules::ax_hal::mem::phys_to_virt((address as usize).into())
                    .as_ptr()
            };
            // SAFETY: This hook runs under the queue owner's publication lock.
            // queue_set supplied live, aligned split-ring RAM. The driver has
            // published one request and retains it until completion; the device
            // only reads these descriptors and the request header.
            unsafe {
                let index = ptr::read_volatile(ram(driver).add(2).cast::<u16>());
                let slot = usize::from(index.wrapping_sub(1)) % size as usize;
                let head = ptr::read_volatile(ram(driver).add(4 + slot * 2).cast::<u16>());
                assert!(u32::from(head) < size);
                let header =
                    ptr::read_volatile(ram(descriptors).add(usize::from(head) * 16).cast::<u64>());
                let header = ram(header as PhysAddr);
                if ptr::read_unaligned(header.cast::<u32>()) == 4 {
                    assert_eq!(
                        ptr::read_unaligned(header.add(8).cast::<u64>()),
                        0,
                        "VirtIO flush sector must be zero"
                    );
                }
            }
        }
        self.inner.notify(queue);
    }
    fn get_status(&self) -> DeviceStatus {
        match &self.fault {
            Fault::RejectFeatures => self.inner.get_status() & !DeviceStatus::FEATURES_OK,
            Fault::StallReset if self.reset_requested => DeviceStatus::DRIVER_OK,
            Fault::DelayReset(reads) if self.reset_requested && reads.get() != 0 => {
                reads.set(reads.get() - 1);
                DeviceStatus::DRIVER_OK
            }
            _ => self.inner.get_status(),
        }
    }
    fn set_status(&mut self, status: DeviceStatus) {
        if status.is_empty() && self.initialized {
            self.reset_requested = true;
            if matches!(self.fault, Fault::StallReset) {
                return;
            }
        }
        self.initialized |= status.contains(DeviceStatus::DRIVER_OK);
        self.inner.set_status(status);
    }
    fn set_guest_page_size(&mut self, size: u32) {
        self.inner.set_guest_page_size(size);
    }
    fn requires_legacy_layout(&self) -> bool {
        self.inner.requires_legacy_layout()
    }
    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: PhysAddr,
        driver: PhysAddr,
        device: PhysAddr,
    ) {
        self.queue_memory = Some((descriptors, driver, size));
        self.inner
            .queue_set(queue, size, descriptors, driver, device);
    }
    fn queue_unset(&mut self, queue: u16) {
        self.inner.queue_unset(queue);
    }
    fn queue_used(&mut self, queue: u16) -> bool {
        self.inner.queue_used(queue)
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        self.inner.ack_interrupt()
    }
    fn read_config_generation(&self) -> u32 {
        self.inner.read_config_generation()
    }
    fn read_config_space<T: FromBytes + IntoBytes>(
        &self,
        offset: usize,
    ) -> virtio_drivers::Result<T> {
        self.inner.read_config_space(offset)
    }
    fn write_config_space<T: IntoBytes + Immutable>(
        &mut self,
        offset: usize,
        value: T,
    ) -> virtio_drivers::Result<()> {
        self.inner.write_config_space(offset, value)
    }
}
