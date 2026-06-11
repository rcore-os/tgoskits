use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use rdrive::DeviceId as RDriveDeviceId;

use super::manager::UsbFsManager;

static USBFS_MANAGER: LazyInit<Arc<UsbFsManager>> = LazyInit::new();
static USBFS_IRQ_REGISTRY: LazyInit<UsbIrqRegistry> = LazyInit::new();

pub(super) struct PendingUsbIrqSlot {
    pub(super) irq_num: usize,
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) handler: ax_driver::usb::UsbHostIrqHandler,
}

pub(super) struct UsbIrqSlot {
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
        let slot_count = pending_slots
            .iter()
            .map(|slot| slot.irq_num)
            .max()
            .map(|irq| irq + 1)
            .unwrap_or(0);
        let mut slots = (0..slot_count).map(|_| None).collect::<Vec<_>>();
        for slot in pending_slots {
            if slots[slot.irq_num].is_some() {
                warn!(
                    "usbfs: duplicate IRQ {} for USB host {:?}, skipping",
                    slot.irq_num, slot.device_id
                );
                continue;
            }
            slots[slot.irq_num] = Some(UsbIrqSlot {
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

    fn slot(&self, irq_num: usize) -> Option<&UsbIrqSlot> {
        let slots = unsafe { &*self.slots.get() };
        slots.get(irq_num).and_then(Option::as_ref)
    }

    fn iter_irqs(&self) -> impl Iterator<Item = usize> + '_ {
        let slots = unsafe { &*self.slots.get() };
        slots
            .iter()
            .enumerate()
            .filter_map(|(irq_num, slot)| slot.as_ref().map(|_| irq_num))
    }
}

pub(super) fn manager() -> Option<Arc<UsbFsManager>> {
    USBFS_MANAGER.get().map(Arc::clone)
}

pub(super) fn init_globals(manager: Arc<UsbFsManager>, pending_slots: Vec<PendingUsbIrqSlot>) {
    USBFS_MANAGER.init_once(manager);
    USBFS_IRQ_REGISTRY.init_once(UsbIrqRegistry::new(pending_slots));

    if let Some(registry) = USBFS_IRQ_REGISTRY.get() {
        for irq_num in registry.iter_irqs() {
            if let Some(slot) = registry.slot(irq_num) {
                info!(
                    "usbfs: registering IRQ callback for IRQ {} (bus {}, host {:?})",
                    irq_num, slot.bus_num, slot.device_id
                );
            }
            match ax_runtime::hal::irq::request_shared_irq(
                irq_num,
                usbfs_raw_irq_handler,
                NonNull::dangling(),
            ) {
                Ok(handle) => {
                    if let Some(slot) = registry.slot(irq_num) {
                        *slot.handle.lock() = Some(handle);
                    }
                }
                Err(err) => {
                    warn!("usbfs: failed to register IRQ callback for IRQ {irq_num}: {err:?}");
                }
            }
        }
    }
}

pub(super) fn free_irq(irq_num: usize) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some(slot) = registry.slot(irq_num) else {
        return;
    };
    let Some(handle) = slot.handle.lock().take() else {
        return;
    };
    if let Err(err) = ax_runtime::hal::irq::free_irq(handle) {
        warn!("usbfs: failed to free IRQ callback for IRQ {irq_num}: {err:?}");
    }
}

pub(super) fn take_dirty(irq_num: usize) -> bool {
    USBFS_IRQ_REGISTRY
        .get()
        .and_then(|registry| registry.slot(irq_num))
        .map(|slot| slot.dirty.swap(false, Ordering::AcqRel))
        .unwrap_or(false)
}

pub(super) fn bootstrap_irq(irq_num: usize) {
    usbfs_irq_handler(irq_num);
}

fn usbfs_irq_handler(irq_num: usize) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some(slot) = registry.slot(irq_num) else {
        warn!("usbfs: no IRQ slot registered for IRQ {}", irq_num);
        return;
    };

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
                    irq_num, slot.bus_num, slot.device_id, port
                );
            }
            crab_usb::Event::TransferActivity { count } => {
                transfer_events += count;
                trace!(
                    "usbfs: IRQ {} bus {} host {:?}: {} transfer event(s)",
                    irq_num, slot.bus_num, slot.device_id, count
                );
            }
            crab_usb::Event::Stopped => {
                stopped_events += 1;
                trace!(
                    "usbfs: IRQ {} bus {} host {:?}: event handler stopped",
                    irq_num, slot.bus_num, slot.device_id
                );
            }
            crab_usb::Event::Nothing => break,
        }
    }

    trace!(
        "usbfs: IRQ {} bus {} host {:?}: handled calls={} port_events={} transfer_events={} \
         stopped_events={}",
        irq_num,
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
            manager.notify_usb_activity();
        }
        if has_topology_event {
            slot.dirty.store(true, Ordering::Release);
            manager.refresh_event.notify(1);
        }
    }
}

unsafe fn usbfs_raw_irq_handler(
    ctx: ax_runtime::hal::irq::IrqContext,
    _data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    usbfs_irq_handler(ctx.irq.0);
    ax_runtime::hal::irq::IrqReturn::Handled
}
