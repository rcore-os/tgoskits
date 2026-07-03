use alloc::{borrow::ToOwned, boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_runtime::hal::irq::{AutoEnable, IrqId, IrqRequest, ShareMode};
use rdrive::DeviceId as RDriveDeviceId;

use super::manager::UsbFsManager;

const USBFS_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(1);

static USBFS_MANAGER: LazyInit<Arc<UsbFsManager>> = LazyInit::new();
static USBFS_IRQ_REGISTRY: LazyInit<UsbIrqRegistry> = LazyInit::new();
static USBFS_EVENT_PUMP_STARTED: AtomicBool = AtomicBool::new(false);

pub(super) struct PendingUsbIrqSlot {
    pub(super) irq: Option<IrqId>,
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) handler: ax_driver::usb::UsbHostIrqHandler,
}

pub(super) struct UsbIrqSlot {
    irq: Option<IrqId>,
    device_id: RDriveDeviceId,
    bus_num: u8,
    handler: ax_driver::usb::UsbHostIrqHandler,
    dirty: AtomicBool,
    handle: SpinNoIrq<Option<ax_runtime::hal::irq::IrqHandle>>,
}

pub(super) struct UsbIrqRegistry {
    slots: UnsafeCell<Box<[Option<UsbIrqSlot>]>>,
}

unsafe impl Sync for UsbIrqRegistry {}

impl UsbIrqRegistry {
    fn new(pending_slots: Vec<PendingUsbIrqSlot>) -> Self {
        let slot_count = pending_slots.len();
        let mut slots = (0..slot_count).map(|_| None).collect::<Vec<_>>();
        for (slot_index, slot) in pending_slots.into_iter().enumerate() {
            slots[slot_index] = Some(UsbIrqSlot {
                irq: slot.irq,
                device_id: slot.device_id,
                bus_num: slot.bus_num,
                handler: slot.handler,
                dirty: AtomicBool::new(false),
                handle: SpinNoIrq::new(None),
            });
        }
        Self {
            slots: UnsafeCell::new(slots.into_boxed_slice()),
        }
    }

    fn slot(&self, slot_index: usize) -> Option<&UsbIrqSlot> {
        let slots = unsafe { &*self.slots.get() };
        slots.get(slot_index).and_then(Option::as_ref)
    }

    fn iter_slots(&self) -> impl Iterator<Item = (usize, &UsbIrqSlot)> + '_ {
        let slots = unsafe { &*self.slots.get() };
        slots
            .iter()
            .enumerate()
            .filter_map(|(slot_index, slot)| slot.as_ref().map(|slot| (slot_index, slot)))
    }

    fn slot_by_irq(&self, irq: IrqId) -> Option<&UsbIrqSlot> {
        self.iter_slots()
            .find_map(|(_, slot)| (slot.irq == Some(irq)).then_some(slot))
    }
}

pub(super) fn manager() -> Option<Arc<UsbFsManager>> {
    USBFS_MANAGER.get().map(Arc::clone)
}

pub(super) fn init_globals(manager: Arc<UsbFsManager>, pending_slots: Vec<PendingUsbIrqSlot>) {
    USBFS_MANAGER.init_once(manager);
    USBFS_IRQ_REGISTRY.init_once(UsbIrqRegistry::new(pending_slots));

    if let Some(registry) = USBFS_IRQ_REGISTRY.get() {
        for (_, slot) in registry.iter_slots() {
            if let Some(irq) = slot.irq {
                info!(
                    "usbfs: registering IRQ callback for IRQ {:?} (bus {}, host {:?})",
                    irq, slot.bus_num, slot.device_id
                );
                let request = IrqRequest::new(|ctx| {
                    usbfs_irq_handler_by_irq(ctx.irq);
                    ax_runtime::hal::irq::IrqReturn::Handled
                })
                .share_mode(ShareMode::Shared)
                .auto_enable(AutoEnable::No);
                match ax_runtime::hal::irq::request_irq(irq, request) {
                    Ok(handle) => {
                        *slot.handle.lock() = Some(handle);
                    }
                    Err(err) => {
                        warn!("usbfs: failed to register IRQ callback for IRQ {irq:?}: {err:?}");
                    }
                }
            } else {
                info!(
                    "usbfs: polling event handler for bus {} host {:?}",
                    slot.bus_num, slot.device_id
                );
            }
        }
    }
}

pub(super) fn start_event_pump() {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    if !registry.iter_slots().any(|(_, slot)| slot.irq.is_none()) {
        return;
    }
    if USBFS_EVENT_PUMP_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    ax_task::spawn_with_name(
        move || {
            loop {
                usbfs_poll_events();
                ax_task::sleep(USBFS_EVENT_POLL_INTERVAL);
            }
        },
        "usbfs-event-pump".to_owned(),
    );
}

pub(super) fn free_irq(irq: IrqId) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some(slot) = registry.slot_by_irq(irq) else {
        return;
    };
    let Some(handle) = slot.handle.lock().take() else {
        return;
    };
    if let Err(err) = ax_runtime::hal::irq::free_irq(handle) {
        warn!("usbfs: failed to free IRQ callback for IRQ {irq:?}: {err:?}");
    }
}

pub(super) fn enable_irq(irq: IrqId) -> bool {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return false;
    };
    let Some(slot) = registry.slot_by_irq(irq) else {
        return false;
    };
    let Some(handle) = *slot.handle.lock() else {
        return false;
    };
    match ax_runtime::hal::irq::enable_irq(handle) {
        Ok(()) => true,
        Err(err) => {
            warn!("usbfs: failed to enable IRQ callback for IRQ {irq:?}: {err:?}");
            false
        }
    }
}

pub(super) fn take_dirty(irq: IrqId) -> bool {
    USBFS_IRQ_REGISTRY
        .get()
        .and_then(|registry| registry.slot_by_irq(irq))
        .map(|slot| slot.dirty.swap(false, Ordering::AcqRel))
        .unwrap_or(false)
}

pub(super) fn bootstrap_irq(irq: IrqId) {
    usbfs_irq_handler_by_irq(irq);
}

fn usbfs_poll_events() {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    for (slot_index, slot) in registry.iter_slots() {
        if slot.irq.is_some() {
            continue;
        }
        usbfs_event_handler(slot_index);
    }
}

fn usbfs_irq_handler_by_irq(irq: IrqId) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some((slot_index, _)) = registry
        .iter_slots()
        .find(|(_, slot)| slot.irq == Some(irq))
    else {
        warn!("usbfs: no IRQ slot registered for IRQ {:?}", irq);
        return;
    };
    usbfs_event_handler(slot_index);
}

fn usbfs_event_handler(slot_index: usize) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some(slot) = registry.slot(slot_index) else {
        return;
    };
    let irq_name = slot
        .irq
        .map(|irq| alloc::format!("{irq:?}"))
        .unwrap_or_else(|| "poll".to_owned());

    let mut handler_calls = 0usize;
    let mut port_events = 0usize;
    let mut transfer_events = 0usize;
    let mut stopped_events = 0usize;
    loop {
        handler_calls += 1;
        match slot.handler.handle() {
            crab_usb::Event::PortChange { port } => {
                port_events += 1;
                trace!(
                    "usbfs: IRQ {} bus {} host {:?}: port change on port {}",
                    irq_name, slot.bus_num, slot.device_id, port
                );
            }
            crab_usb::Event::TransferActivity { count } => {
                transfer_events += count;
                trace!(
                    "usbfs: IRQ {} bus {} host {:?}: {} transfer event(s)",
                    irq_name, slot.bus_num, slot.device_id, count
                );
            }
            crab_usb::Event::Stopped => {
                stopped_events += 1;
                trace!(
                    "usbfs: IRQ {} bus {} host {:?}: event handler stopped",
                    irq_name, slot.bus_num, slot.device_id
                );
            }
            crab_usb::Event::Nothing => break,
        }
    }

    trace!(
        "usbfs: IRQ {} bus {} host {:?}: handled calls={} port_events={} transfer_events={} \
         stopped_events={}",
        irq_name,
        slot.bus_num,
        slot.device_id,
        handler_calls,
        port_events,
        transfer_events,
        stopped_events
    );

    let has_topology_event = port_events > 0 || stopped_events > 0;
    let has_usb_activity = has_topology_event || transfer_events > 0;

    if let Some(manager) = USBFS_MANAGER.get() {
        if has_usb_activity {
            manager.notify_usb_activity_from_irq();
        }
        if has_topology_event {
            slot.dirty.store(true, Ordering::Release);
            manager.notify_topology_from_irq();
        }
    }
}
