use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_lazyinit::LazyInit;
use crab_usb::{Event, EventHandler};
use rdrive::DeviceId as RDriveDeviceId;

use super::manager::UsbFsManager;

static USBFS_MANAGER: LazyInit<Arc<UsbFsManager>> = LazyInit::new();
static USBFS_IRQ_REGISTRY: LazyInit<UsbIrqRegistry> = LazyInit::new();

pub(super) struct PendingUsbIrqSlot {
    pub(super) irq_num: usize,
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) handler: EventHandler,
}

pub(super) struct UsbIrqSlot {
    device_id: RDriveDeviceId,
    bus_num: u8,
    handler: EventHandler,
    dirty: AtomicBool,
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
            if !ax_hal::irq::register(irq_num, usbfs_irq_handler) {
                warn!("usbfs: failed to register IRQ callback for IRQ {}", irq_num);
            }
        }
    }
}

pub(super) fn take_dirty(irq_num: usize) -> bool {
    USBFS_IRQ_REGISTRY
        .get()
        .and_then(|registry| registry.slot(irq_num))
        .map(|slot| slot.dirty.swap(false, Ordering::AcqRel))
        .unwrap_or(false)
}

fn usbfs_irq_handler(irq_num: usize) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some(slot) = registry.slot(irq_num) else {
        warn!("usbfs: no IRQ slot registered for IRQ {}", irq_num);
        return;
    };

    trace!(
        "usbfs: handling IRQ {} for bus {} host {:?}",
        irq_num, slot.bus_num, slot.device_id
    );

    while let Event::PortChange { .. } | Event::Stopped = slot.handler.handle_event() {}
    slot.dirty.store(true, Ordering::Release);

    if let Some(manager) = USBFS_MANAGER.get() {
        manager.refresh_event.notify(1);
    }
}
