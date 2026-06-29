extern crate alloc;

use alloc::{borrow::ToOwned, format, string::String};

use rdif_input::{AbsInfo, Event, EventType, InputDeviceId, InputError, InputEvent};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::input::{InputConfigSelect, VirtIOInput},
    transport::{InterruptStatus, Transport},
};

use crate::{BindingInfo, input::PlatformDeviceInput, virtio::VirtIoHalImpl};
#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
use crate::{PciIrqRequirement, binding_info_from_pci};

#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
crate::model_register!(
    name: "VirtIO Input",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let transport =
        crate::pci::take_virtio_transport_masked(probe.endpoint_mut(), DeviceType::Input)?;
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Optional)?;
    register_transport_with_info(probe.into_platform_device(), transport, info)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, BindingInfo::empty())
}

pub fn register_transport_with_info<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let irq_num = info.irq_num();
    let dev = VirtIoInputDevice::new(transport, irq_num).map_err(|err| {
        OnProbeError::other(format!("failed to initialize virtio-input: {err:?}"))
    })?;
    let irq = plat_dev.register_input_with_info(dev, info);
    log::info!("registered virtio input device irq={irq:?}");
    Ok(())
}

struct VirtIoInputDevice<T: Transport + 'static> {
    raw: VirtIOInput<VirtIoHalImpl, T>,
    device_id: InputDeviceId,
    name: String,
    physical_location: String,
    unique_id: String,
    irq_num: Option<usize>,
    irq_enabled: bool,
}

unsafe impl<T: Transport + 'static> Send for VirtIoInputDevice<T> {}

impl<T: Transport + 'static> VirtIoInputDevice<T> {
    fn new(transport: T, irq_num: Option<usize>) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOInput::new(transport)?;
        let name = raw.name().unwrap_or_else(|_| "<unknown>".to_owned());
        let id = raw.ids()?;
        let device_id = InputDeviceId {
            bus_type: id.bustype,
            vendor: id.vendor,
            product: id.product,
            version: id.version,
        };
        let physical_location = format!(
            "virtio-input/{:04x}:{:04x}:{:04x}:{:04x}",
            device_id.bus_type, device_id.vendor, device_id.product, device_id.version
        );
        let unique_id = format!(
            "virtio-{:04x}-{:04x}-{:04x}-{:04x}-{}",
            device_id.bus_type, device_id.vendor, device_id.product, device_id.version, name
        );
        // Creating the event queue can raise an interrupt before the
        // OS-specific evdev layer has installed its shared IRQ action.
        // Acknowledge only the transport ISR here; queued input events remain
        // in the virtqueue for the first reader to drain.
        let _ = raw.ack_interrupt();

        Ok(Self {
            raw,
            device_id,
            name,
            physical_location,
            unique_id,
            irq_num,
            irq_enabled: false,
        })
    }
}

impl<T: Transport + 'static> DriverGeneric for VirtIoInputDevice<T> {
    fn name(&self) -> &str {
        &self.name
    }
}

impl<T: Transport + 'static> rdif_input::Interface for VirtIoInputDevice<T> {
    fn device_id(&self) -> InputDeviceId {
        self.device_id
    }

    fn physical_location(&self) -> &str {
        &self.physical_location
    }

    fn unique_id(&self) -> &str {
        &self.unique_id
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError> {
        self.raw
            .query_config_select(InputConfigSelect::EvBits, ty as u8, out)
            .map(|read| read != 0)
            .map_err(map_input_err)
    }

    fn read_event(&mut self) -> Result<InputEvent, InputError> {
        self.raw.ack_interrupt();
        let event = self.raw.pop_pending_event().ok_or(InputError::Again)?;
        Ok(InputEvent {
            event_type: event.event_type,
            code: event.code,
            value: event.value as i32,
        })
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> Result<usize, InputError> {
        let bits = self.raw.prop_bits().map_err(map_input_err)?;
        let len = bits.len().min(out.len());
        out[..len].copy_from_slice(&bits[..len]);
        Ok(len)
    }

    fn get_abs_info(&mut self, axis: u8) -> Result<AbsInfo, InputError> {
        let info = self.raw.abs_info(axis).map_err(map_input_err)?;
        Ok(AbsInfo {
            min: info.min as i32,
            max: info.max as i32,
            fuzz: info.fuzz as i32,
            flat: info.flat as i32,
            res: info.res as i32,
        })
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let status = self.raw.ack_interrupt();
        input_irq_event(self.irq_enabled, status)
    }
}

fn input_irq_event(irq_enabled: bool, status: InterruptStatus) -> Event {
    if !irq_enabled {
        return Event::none();
    }
    Event {
        handled: !status.is_empty(),
        input_ready: status.contains(InterruptStatus::QUEUE_INTERRUPT),
    }
}

fn map_input_err(err: VirtIoError) -> InputError {
    match err {
        VirtIoError::Unsupported => InputError::NotSupported,
        VirtIoError::NotReady => InputError::Again,
        _ => InputError::Other(alloc::boxed::Box::new(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_irq_is_ignored_until_driver_enables_it() {
        let status =
            InterruptStatus::QUEUE_INTERRUPT | InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT;

        assert_eq!(input_irq_event(false, status), Event::none());
    }

    #[test]
    fn input_irq_queue_interrupt_makes_input_ready() {
        assert_eq!(
            input_irq_event(true, InterruptStatus::QUEUE_INTERRUPT),
            Event {
                handled: true,
                input_ready: true,
            }
        );
    }

    #[test]
    fn input_irq_configuration_interrupt_is_claimed_without_input_ready() {
        assert_eq!(
            input_irq_event(true, InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
            Event {
                handled: true,
                input_ready: false,
            }
        );
    }

    #[test]
    fn input_irq_empty_status_is_not_claimed() {
        assert_eq!(
            input_irq_event(true, InterruptStatus::empty()),
            Event::none()
        );
    }
}
