use alloc::{borrow::ToOwned, boxed::Box, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_runtime::hal::irq::{AutoEnable, IrqId, IrqRequest, ShareMode};
use rdrive::DeviceId as RDriveDeviceId;

use super::manager::UsbFsManager;
use crate::task::future::IrqNotify;

const USBFS_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(1);
const USBFS_EVENT_BATCH_LIMIT: usize = 64;

static USBFS_MANAGER: LazyInit<Arc<UsbFsManager>> = LazyInit::new();
static USBFS_IRQ_REGISTRY: LazyInit<UsbIrqRegistry> = LazyInit::new();
static USBFS_EVENT_WORKER_STARTED: AtomicBool = AtomicBool::new(false);
static USBFS_POLL_TICKER_STARTED: AtomicBool = AtomicBool::new(false);

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
    active: AtomicBool,
    dirty: AtomicBool,
    deferred: AtomicBool,
    handler_busy: AtomicBool,
    handle: SpinNoIrq<Option<ax_runtime::hal::irq::IrqHandle>>,
}

pub(super) struct UsbIrqRegistry {
    slots: Box<[Option<UsbIrqSlot>]>,
    deferred_notify: IrqNotify,
    service_cursor: AtomicUsize,
}

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
                active: AtomicBool::new(true),
                dirty: AtomicBool::new(false),
                deferred: AtomicBool::new(false),
                handler_busy: AtomicBool::new(false),
                handle: SpinNoIrq::new(None),
            });
        }
        Self {
            slots: slots.into_boxed_slice(),
            deferred_notify: IrqNotify::new(),
            service_cursor: AtomicUsize::new(0),
        }
    }

    fn iter_slots(&self) -> impl Iterator<Item = (usize, &UsbIrqSlot)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot_index, slot)| slot.as_ref().map(|slot| (slot_index, slot)))
    }

    fn slot(&self, slot_index: usize) -> Option<&UsbIrqSlot> {
        self.slots.get(slot_index).and_then(Option::as_ref)
    }
}

pub(super) fn manager() -> Option<Arc<UsbFsManager>> {
    USBFS_MANAGER.get().map(Arc::clone)
}

pub(super) fn init_globals(manager: Arc<UsbFsManager>, pending_slots: Vec<PendingUsbIrqSlot>) {
    USBFS_MANAGER.init_once(manager);
    USBFS_IRQ_REGISTRY.init_once(UsbIrqRegistry::new(pending_slots));

    if let Some(registry) = USBFS_IRQ_REGISTRY.get() {
        for (slot_index, slot) in registry.iter_slots() {
            if let Some(irq) = slot.irq {
                info!(
                    "usbfs: registering IRQ callback for IRQ {:?} (bus {}, host {:?})",
                    irq, slot.bus_num, slot.device_id
                );
                let request = IrqRequest::new(move |_ctx| {
                    usbfs_irq_handler_by_slot(slot_index);
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
    if !registry.iter_slots().any(|(_, slot)| slot.active()) {
        return;
    }
    if USBFS_EVENT_WORKER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        crate::task::spawn_kernel_thread(usbfs_event_service_task, "usbfs-event-worker".to_owned());
        registry.deferred_notify.notify();
    }

    if registry
        .iter_slots()
        .any(|(_, slot)| slot.active() && slot.irq.is_none())
        && USBFS_POLL_TICKER_STARTED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        crate::task::spawn_kernel_thread(usbfs_poll_ticker_task, "usbfs-event-ticker".to_owned());
    }
}

pub(super) fn free_device_irq(device_id: RDriveDeviceId) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    for (_, slot) in registry
        .iter_slots()
        .filter(|(_, slot)| slot.device_id == device_id)
    {
        slot.active.store(false, Ordering::Release);
        slot.deferred.store(false, Ordering::Release);
        let Some(handle) = slot.handle.lock().take() else {
            continue;
        };
        if let Err(err) = ax_runtime::hal::irq::free_irq(handle) {
            warn!(
                "usbfs: failed to free IRQ callback for host {:?}: {err:?}",
                device_id
            );
        }
    }
}

pub(super) fn enable_device_irq(device_id: RDriveDeviceId) -> bool {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return false;
    };
    let mut found = false;
    let mut all_enabled = true;
    for (_, slot) in registry
        .iter_slots()
        .filter(|(_, slot)| slot.device_id == device_id && slot.irq.is_some())
    {
        found = true;
        let Some(handle) = *slot.handle.lock() else {
            all_enabled = false;
            continue;
        };
        if let Err(err) = ax_runtime::hal::irq::enable_irq(handle) {
            warn!(
                "usbfs: failed to enable IRQ callback for host {:?}: {err:?}",
                device_id
            );
            all_enabled = false;
        }
    }
    found && all_enabled
}

pub(super) fn take_dirty_for_device(device_id: RDriveDeviceId) -> bool {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return false;
    };
    let mut dirty = false;
    for (_, slot) in registry
        .iter_slots()
        .filter(|(_, slot)| slot.active() && slot.device_id == device_id)
    {
        dirty |= slot.dirty.swap(false, Ordering::AcqRel);
    }
    dirty
}

pub(super) fn disable_device(device_id: RDriveDeviceId) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    for (_, slot) in registry
        .iter_slots()
        .filter(|(_, slot)| slot.device_id == device_id)
    {
        slot.active.store(false, Ordering::Release);
        slot.deferred.store(false, Ordering::Release);
        slot.dirty.store(false, Ordering::Release);
    }
}

pub(super) fn bootstrap_device(device_id: RDriveDeviceId) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    for (_, slot) in registry
        .iter_slots()
        .filter(|(_, slot)| slot.device_id == device_id && slot.active())
    {
        usbfs_event_handler(slot);
    }
}

fn usbfs_irq_handler_by_slot(slot_index: usize) {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let Some(slot) = registry.slot(slot_index) else {
        return;
    };
    usbfs_event_handler(slot);
}

fn usbfs_event_handler(slot: &UsbIrqSlot) {
    if !slot.active() {
        return;
    }
    if slot
        .handler_busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        defer_event_drain(slot);
        return;
    }
    if !slot.active() {
        slot.handler_busy.store(false, Ordering::Release);
        return;
    }

    let mut port_events = 0usize;
    let mut transfer_events = 0usize;
    let mut stopped_events = 0usize;
    let mut exhausted = true;
    for _ in 0..USBFS_EVENT_BATCH_LIMIT {
        match slot.handler.handle() {
            crab_usb::Event::PortChange { .. } => {
                port_events = port_events.saturating_add(1);
            }
            crab_usb::Event::TransferActivity { count } => {
                transfer_events = transfer_events.saturating_add(count);
            }
            crab_usb::Event::Stopped => {
                stopped_events = stopped_events.saturating_add(1);
            }
            crab_usb::Event::Nothing => {
                exhausted = false;
                break;
            }
        }
    }
    slot.handler_busy.store(false, Ordering::Release);

    let has_topology_event = port_events > 0 || stopped_events > 0;
    let has_usb_activity = has_topology_event || transfer_events > 0;

    if has_topology_event {
        slot.dirty.store(true, Ordering::Release);
    }
    if let Some(manager) = USBFS_MANAGER.get()
        && has_usb_activity
    {
        manager.notify_usb_activity_from_irq();
    }
    if exhausted {
        defer_event_drain(slot);
    }
}

fn defer_event_drain(slot: &UsbIrqSlot) {
    if !slot.active() {
        return;
    }
    slot.deferred.store(true, Ordering::Release);
    if let Some(registry) = USBFS_IRQ_REGISTRY.get() {
        registry.deferred_notify.notify_irq();
    }
}

fn service_deferred_events() {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    let slot_count = registry.slots.len();
    if slot_count == 0 {
        return;
    }

    let start = registry.service_cursor.load(Ordering::Acquire) % slot_count;
    for offset in 0..slot_count {
        let slot_index = (start + offset) % slot_count;
        let Some(slot) = registry.slot(slot_index) else {
            continue;
        };
        let is_polling_host = slot.irq.is_none();
        let is_deferred = slot.deferred.swap(false, Ordering::AcqRel);
        if slot.active() && (is_polling_host || is_deferred) {
            registry
                .service_cursor
                .store((slot_index + 1) % slot_count, Ordering::Release);
            usbfs_event_handler(slot);
            break;
        }
    }

    if registry
        .iter_slots()
        .any(|(_, slot)| slot.active() && slot.deferred.load(Ordering::Acquire))
    {
        registry.deferred_notify.notify();
    }
}

fn usbfs_event_service_task() {
    let registry = USBFS_IRQ_REGISTRY
        .get()
        .unwrap_or_else(|| unreachable!("USB event worker starts after registry initialization"));
    loop {
        registry.deferred_notify.wait();
        service_deferred_events();
        crate::task::yield_now();
    }
}

fn usbfs_poll_ticker_task() {
    let registry = USBFS_IRQ_REGISTRY
        .get()
        .unwrap_or_else(|| unreachable!("USB poll ticker starts after registry initialization"));
    loop {
        if !registry
            .iter_slots()
            .any(|(_, slot)| slot.active() && slot.irq.is_none())
        {
            return;
        }
        registry.deferred_notify.notify();
        crate::task::sleep(USBFS_EVENT_POLL_INTERVAL);
    }
}

impl UsbIrqSlot {
    fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}
