#[cfg(any(kmod, umod))]
use alloc::boxed::Box;
#[cfg(kmod)]
use core::sync::atomic::{AtomicU64, Ordering};
use core::{any::Any, fmt::Debug};

use futures::future::BoxFuture;
#[cfg(kmod)]
use rdif_irq::{ContainmentCause, IrqCapture, MaskedSource};
use usb_if::descriptor::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor};

use crate::{backend::ty::ep::Endpoint, err::USBError};

pub mod ep;
#[cfg(any(kmod, umod))]
pub mod transfer;

#[derive(Debug, Clone)]
pub enum Event {
    Nothing,
    PortChange { port: u8 },
    TransferActivity { count: usize },
    Stopped,
}

/// Stable acknowledgement of one USB host interrupt source.
///
/// The hard-IRQ endpoint masks the source before publishing this value. Only
/// the CPU-pinned host owner may consume it and rearm the matching generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(kmod)]
pub struct UsbIrqEvent {
    generation: u64,
    sources: u64,
}

#[cfg(kmod)]
impl UsbIrqEvent {
    pub(crate) const fn new(generation: u64, sources: u64) -> Self {
        Self {
            generation,
            sources,
        }
    }

    /// Returns the source-generation captured before the action returned.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the driver-private source bitmap captured from hardware.
    pub const fn sources(self) -> u64 {
        self.sources
    }
}

/// Failure while capturing, servicing, or rearming a USB IRQ source.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[cfg(kmod)]
pub enum UsbIrqFault {
    /// A second event reached an endpoint whose previous source is still masked.
    #[error("USB IRQ source still owns generation {0}")]
    SourceBusy(u64),
    /// A stale or foreign event was supplied to the maintenance owner.
    #[error("stale USB IRQ event generation: expected {expected}, got {actual}")]
    StaleEvent { expected: u64, actual: u64 },
    /// A stale, replayed, or partially overlapping source token was supplied.
    #[error("stale USB IRQ rearm token")]
    StaleRearm,
    /// The source-generation counter wrapped through its reserved zero value.
    #[error("USB IRQ source generation exhausted")]
    GenerationExhausted,
}

/// Generation state shared only by one same-CPU IRQ/maintenance endpoint.
#[cfg(kmod)]
pub(crate) struct IrqEpoch {
    next_generation: AtomicU64,
    active_generation: AtomicU64,
    active_sources: AtomicU64,
}

#[cfg(kmod)]
impl IrqEpoch {
    pub(crate) const fn new() -> Self {
        Self {
            next_generation: AtomicU64::new(1),
            active_generation: AtomicU64::new(0),
            active_sources: AtomicU64::new(0),
        }
    }

    pub(crate) fn capture(&self, sources: u64) -> Result<(UsbIrqEvent, MaskedSource), UsbIrqFault> {
        let active = self.active_generation.load(Ordering::Acquire);
        if active != 0 {
            return Err(UsbIrqFault::SourceBusy(active));
        }
        let generation = self.next_generation.fetch_add(1, Ordering::AcqRel);
        if generation == 0 {
            return Err(UsbIrqFault::GenerationExhausted);
        }
        let source =
            MaskedSource::try_new(generation, sources).map_err(|_| UsbIrqFault::StaleRearm)?;
        self.active_sources.store(sources, Ordering::Relaxed);
        self.active_generation.store(generation, Ordering::Release);
        Ok((UsbIrqEvent::new(generation, sources), source))
    }

    pub(crate) fn contained_or_capture(&self, sources: u64) -> Result<MaskedSource, UsbIrqFault> {
        let generation = self.active_generation.load(Ordering::Acquire);
        if generation != 0 {
            let active_sources = self.active_sources.load(Ordering::Relaxed);
            return MaskedSource::try_new(generation, active_sources)
                .map_err(|_| UsbIrqFault::StaleRearm);
        }
        self.capture(sources).map(|(_, source)| source)
    }

    pub(crate) fn validate_event(&self, event: UsbIrqEvent) -> Result<(), UsbIrqFault> {
        let expected = self.active_generation.load(Ordering::Acquire);
        if expected != event.generation
            || self.active_sources.load(Ordering::Relaxed) != event.sources
        {
            return Err(UsbIrqFault::StaleEvent {
                expected,
                actual: event.generation,
            });
        }
        Ok(())
    }

    pub(crate) fn finish_rearm(&self, source: MaskedSource) -> Result<(), UsbIrqFault> {
        let expected = self.active_generation.load(Ordering::Acquire);
        if expected != source.generation().get()
            || self.active_sources.load(Ordering::Relaxed) != source.bitmap().get()
        {
            return Err(UsbIrqFault::StaleRearm);
        }
        self.active_sources.store(0, Ordering::Relaxed);
        self.active_generation.store(0, Ordering::Release);
        Ok(())
    }
}

#[cfg(kmod)]
pub(crate) trait EventHandlerOp: Send + Any + Sync + 'static {
    fn capture_irq(&self) -> IrqCapture<UsbIrqEvent, UsbIrqFault>;

    fn service_host_events(&self, event: UsbIrqEvent) -> Result<Event, UsbIrqFault>;

    fn contain(&self, cause: ContainmentCause) -> Result<MaskedSource, UsbIrqFault>;

    fn rearm_sources(&self, source: MaskedSource) -> Result<(), UsbIrqFault>;
}

#[allow(dead_code)]
pub(crate) trait DeviceInfoOp: Send + Sync + Any + Debug + 'static {
    fn id(&self) -> usize;
    fn backend_name(&self) -> &str;
    fn descriptor(&self) -> &DeviceDescriptor;
    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor];
}

#[cfg(any(kmod, umod))]
pub enum ProbedDeviceInfoOp {
    Device(Box<dyn DeviceInfoOp>),
    Hub(Box<dyn DeviceInfoOp>),
}

/// USB 设备特征（高层抽象）
pub(crate) trait DeviceOp: Send + Any + 'static {
    fn id(&self) -> usize;
    fn backend_name(&self) -> &str;
    fn descriptor(&self) -> &DeviceDescriptor;
    fn configuration_descriptors(&self) -> &[ConfigurationDescriptor];

    fn ctrl_ep_ref(&self) -> &Endpoint;

    fn ctrl_ep_mut(&mut self) -> &mut Endpoint;

    fn claim_interface<'a>(
        &'a mut self,
        interface: u8,
        alternate: u8,
    ) -> BoxFuture<'a, Result<(), USBError>>;

    fn set_configuration<'a>(
        &'a mut self,
        configuration_value: u8,
    ) -> BoxFuture<'a, Result<(), USBError>>;

    fn endpoint(&mut self, desc: &EndpointDescriptor) -> Result<ep::Endpoint, USBError>;

    fn update_hub(&mut self, params: HubParams) -> BoxFuture<'_, Result<(), USBError>>;
}

#[cfg(all(test, kmod))]
mod tests {
    use super::*;

    #[test]
    fn irq_generation_keeps_one_masked_source_until_owner_rearm() {
        let epoch = IrqEpoch::new();
        let (event, source) = epoch.capture(0b101).expect("first capture owns source");

        assert_eq!(epoch.validate_event(event), Ok(()));
        assert_eq!(epoch.capture(0b001), Err(UsbIrqFault::SourceBusy(1)));
        assert_eq!(epoch.finish_rearm(source), Ok(()));

        let (next, _) = epoch.capture(0b001).expect("rearm releases next capture");
        assert_eq!(next.generation(), 2);
    }

    #[test]
    fn stale_event_and_rearm_never_reopen_a_new_generation() {
        let epoch = IrqEpoch::new();
        let (first_event, first_source) = epoch.capture(1).unwrap();
        epoch.finish_rearm(first_source).unwrap();
        let (second_event, second_source) = epoch.capture(2).unwrap();

        assert_eq!(
            epoch.validate_event(first_event),
            Err(UsbIrqFault::StaleEvent {
                expected: second_event.generation(),
                actual: first_event.generation(),
            })
        );
        assert_eq!(
            epoch.finish_rearm(first_source),
            Err(UsbIrqFault::StaleRearm)
        );
        assert_eq!(epoch.validate_event(second_event), Ok(()));
        assert_eq!(epoch.finish_rearm(second_source), Ok(()));
    }
}

#[derive(Debug, Clone)]
pub struct HubParams {
    /// Hub 端口数量
    pub num_ports: u8,

    /// 是否为 Multi-TT Hub
    pub multi_tt: bool,

    /// TT 思考时间（单位：纳秒）
    /// 8 FS bit times = 666ns
    pub tt_think_time_ns: u16,

    /// 父 Hub Slot ID（0 表示 Root Hub）
    pub parent_hub_slot_id: u8,

    /// Root Hub 端口号
    pub root_hub_port_number: u8,
}
