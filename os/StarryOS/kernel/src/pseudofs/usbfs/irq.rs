use alloc::{borrow::ToOwned, boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_runtime::hal::irq::{AutoEnable, IrqId, IrqRequest, ShareMode};
use rdrive::DeviceId as RDriveDeviceId;

use super::manager::UsbFsManager;
use crate::task::future::IrqNotify;

const USBFS_EVENT_BATCH_LIMIT: usize = 64;
const USB_EVENT_ACTIVE: u8 = 1 << 0;
const USB_EVENT_BUSY: u8 = 1 << 1;
const USB_EVENT_DEFERRED: u8 = 1 << 2;

static USBFS_MANAGER: LazyInit<Arc<UsbFsManager>> = LazyInit::new();
static USBFS_IRQ_REGISTRY: LazyInit<UsbIrqRegistry> = LazyInit::new();
static USBFS_EVENT_WORKER_STARTED: AtomicBool = AtomicBool::new(false);

pub(super) struct PendingUsbIrqSlot {
    pub(super) irq: IrqId,
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) handler: ax_driver::usb::UsbHostIrqHandler,
}

pub(super) struct UsbIrqSlot {
    irq: IrqId,
    device_id: RDriveDeviceId,
    bus_num: u8,
    handler: ax_driver::usb::UsbHostIrqHandler,
    event_gate: UsbEventGate,
    dirty: AtomicBool,
    handle: SpinNoIrq<Option<ax_runtime::hal::irq::IrqHandle>>,
}

struct UsbEventGate {
    state: AtomicU8,
}

enum UsbEventEntry<'a> {
    Acquired(UsbEventPermit<'a>),
    Busy,
    Inactive,
}

struct UsbEventPermit<'a> {
    gate: &'a UsbEventGate,
}

impl UsbEventGate {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    fn activate(&self) {
        debug_assert!(self.is_quiescent());
        self.state.store(USB_EVENT_ACTIVE, Ordering::Release);
    }

    fn try_enter(&self) -> UsbEventEntry<'_> {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & USB_EVENT_ACTIVE == 0 {
                return UsbEventEntry::Inactive;
            }
            if state & USB_EVENT_BUSY != 0 {
                return UsbEventEntry::Busy;
            }
            match self.state.compare_exchange_weak(
                state,
                state | USB_EVENT_BUSY,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return UsbEventEntry::Acquired(UsbEventPermit { gate: self }),
                Err(observed) => state = observed,
            }
        }
    }

    fn defer(&self) -> bool {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & USB_EVENT_ACTIVE != 0).then_some(state | USB_EVENT_DEFERRED)
            })
            .is_ok()
    }

    fn take_deferred(&self) -> bool {
        self.state.fetch_and(!USB_EVENT_DEFERRED, Ordering::AcqRel) & USB_EVENT_DEFERRED != 0
    }

    fn has_deferred(&self) -> bool {
        self.state.load(Ordering::Acquire) & USB_EVENT_DEFERRED != 0
    }

    fn deactivate(&self) {
        self.state
            .fetch_and(!(USB_EVENT_ACTIVE | USB_EVENT_DEFERRED), Ordering::AcqRel);
    }

    fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) & USB_EVENT_ACTIVE != 0
    }

    fn is_quiescent(&self) -> bool {
        self.state.load(Ordering::Acquire) & USB_EVENT_BUSY == 0
    }
}

impl Drop for UsbEventPermit<'_> {
    fn drop(&mut self) {
        self.gate
            .state
            .fetch_and(!USB_EVENT_BUSY, Ordering::Release);
    }
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
                event_gate: UsbEventGate::new(),
                dirty: AtomicBool::new(false),
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
            let irq = slot.irq;
            info!(
                "usbfs: registering IRQ callback for IRQ {:?} (bus {}, host {:?})",
                irq, slot.bus_num, slot.device_id
            );
            let request =
                IrqRequest::new(move |_ctx| usb_irq_return(usbfs_irq_handler_by_slot(slot_index)))
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
        }
    }
}

pub(super) fn start_event_pump() {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return;
    };
    if registry.slots.is_empty() {
        return;
    }
    if USBFS_EVENT_WORKER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        crate::task::spawn_kernel_thread(usbfs_event_service_task, "usbfs-event-worker".to_owned());
        registry.deferred_notify.notify();
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
        if let Some(handle) = slot.handle.lock().take()
            && let Err(err) = ax_runtime::hal::irq::free_irq(handle)
        {
            warn!("usbfs: failed to free IRQ callback for host {device_id:?}: {err:?}");
        }
    }
}

fn enable_actions_transactionally<T: Copy>(
    handles: impl IntoIterator<Item = Option<T>>,
    mut enable: impl FnMut(T) -> bool,
    mut disable: impl FnMut(T),
) -> bool {
    let mut enabled = Vec::new();
    for handle in handles {
        let Some(handle) = handle else {
            for handle in enabled.into_iter().rev() {
                disable(handle);
            }
            return false;
        };
        if !enable(handle) {
            for handle in enabled.into_iter().rev() {
                disable(handle);
            }
            return false;
        }
        enabled.push(handle);
    }
    !enabled.is_empty()
}

pub(super) fn enable_device_irq(device_id: RDriveDeviceId) -> bool {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return false;
    };
    let slots = registry
        .iter_slots()
        .filter(|(_, slot)| slot.device_id == device_id)
        .map(|(_, slot)| slot)
        .collect::<Vec<_>>();
    for slot in &slots {
        slot.event_gate.activate();
    }

    let enabled = enable_actions_transactionally(
        slots.iter().map(|slot| *slot.handle.lock()),
        |handle| match ax_runtime::hal::irq::enable_irq(handle) {
            Ok(()) => true,
            Err(err) => {
                warn!(
                    "usbfs: failed to enable IRQ callback for host {:?}: {err:?}",
                    device_id
                );
                false
            }
        },
        |handle| {
            if let Err(err) = ax_runtime::hal::irq::disable_irq(handle) {
                warn!(
                    "usbfs: failed to roll back IRQ callback for host {:?}: {err:?}",
                    device_id
                );
            }
        },
    );
    if !enabled {
        for slot in slots {
            slot.event_gate.deactivate();
            slot.dirty.store(false, Ordering::Release);
            wait_for_event_handler(slot);
        }
    }
    enabled
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
        if let Some(handle) = *slot.handle.lock()
            && let Err(err) = ax_runtime::hal::irq::disable_irq(handle)
        {
            warn!("usbfs: failed to disable IRQ callback for host {device_id:?}: {err:?}");
        }
        slot.event_gate.deactivate();
        slot.dirty.store(false, Ordering::Release);
        wait_for_event_handler(slot);
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

fn usbfs_irq_handler_by_slot(slot_index: usize) -> bool {
    let Some(registry) = USBFS_IRQ_REGISTRY.get() else {
        return false;
    };
    let Some(slot) = registry.slot(slot_index) else {
        return false;
    };
    let _permit = match slot.event_gate.try_enter() {
        UsbEventEntry::Acquired(permit) => permit,
        UsbEventEntry::Busy => {
            defer_event_drain(slot);
            return false;
        }
        UsbEventEntry::Inactive => return false,
    };
    if slot.handler.acknowledge_irq() {
        defer_event_drain(slot);
        true
    } else {
        false
    }
}

fn usb_irq_return(handled: bool) -> ax_runtime::hal::irq::IrqReturn {
    if handled {
        ax_runtime::hal::irq::IrqReturn::Handled
    } else {
        ax_runtime::hal::irq::IrqReturn::Unhandled
    }
}

fn usbfs_event_handler(slot: &UsbIrqSlot) {
    let permit = match slot.event_gate.try_enter() {
        UsbEventEntry::Acquired(permit) => permit,
        UsbEventEntry::Busy => {
            defer_event_drain(slot);
            return;
        }
        UsbEventEntry::Inactive => return,
    };
    if !slot.active() {
        return;
    }

    let _acknowledged = slot.handler.acknowledge_irq();
    let batch = drain_event_batch(|| slot.handler.drain_event());
    let has_topology_event = batch.port_events > 0 || batch.stopped_events > 0;
    let has_usb_activity = has_topology_event || batch.transfer_events > 0;

    if has_topology_event {
        slot.dirty.store(true, Ordering::Release);
    }
    if let Some(manager) = USBFS_MANAGER.get()
        && has_usb_activity
    {
        manager.notify_usb_activity_from_irq();
    }
    finish_event_drain(
        permit,
        batch.exhausted,
        || defer_event_drain(slot),
        || {
            if slot.active() {
                slot.handler.rearm_irq();
            }
        },
    );
}

fn finish_event_drain(
    permit: UsbEventPermit<'_>,
    exhausted: bool,
    defer: impl FnOnce(),
    rearm: impl FnOnce(),
) {
    drop(permit);
    if exhausted {
        defer();
    } else {
        rearm();
    }
}

#[derive(Default)]
struct UsbEventBatch {
    port_events: usize,
    transfer_events: usize,
    stopped_events: usize,
    exhausted: bool,
}

fn drain_event_batch(mut next_event: impl FnMut() -> crab_usb::Event) -> UsbEventBatch {
    let mut batch = UsbEventBatch {
        exhausted: true,
        ..UsbEventBatch::default()
    };
    for _ in 0..USBFS_EVENT_BATCH_LIMIT {
        match next_event() {
            crab_usb::Event::PortChange { .. } => {
                batch.port_events = batch.port_events.saturating_add(1);
            }
            crab_usb::Event::TransferActivity { count } => {
                batch.transfer_events = batch.transfer_events.saturating_add(count);
            }
            crab_usb::Event::Stopped => {
                batch.stopped_events = batch.stopped_events.saturating_add(1);
            }
            crab_usb::Event::Nothing => {
                batch.exhausted = false;
                break;
            }
        }
    }
    batch
}

fn defer_event_drain(slot: &UsbIrqSlot) {
    if !slot.event_gate.defer() {
        return;
    }
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
        let is_deferred = slot.event_gate.take_deferred();
        if slot.active() && is_deferred {
            registry
                .service_cursor
                .store((slot_index + 1) % slot_count, Ordering::Release);
            usbfs_event_handler(slot);
            break;
        }
    }

    if registry
        .iter_slots()
        .any(|(_, slot)| slot.active() && slot.event_gate.has_deferred())
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
    }
}

impl UsbIrqSlot {
    fn active(&self) -> bool {
        self.event_gate.is_active()
    }
}

fn wait_for_event_handler(slot: &UsbIrqSlot) {
    while !slot.event_gate.is_quiescent() {
        crate::task::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_rejects_new_event_work_and_waits_for_the_active_permit() {
        let gate = UsbEventGate::new();
        gate.activate();
        let permit = match gate.try_enter() {
            UsbEventEntry::Acquired(permit) => permit,
            _ => panic!("an active USB event gate must issue its first permit"),
        };

        gate.deactivate();

        assert!(!gate.is_quiescent());
        assert!(matches!(gate.try_enter(), UsbEventEntry::Inactive));
        drop(permit);
        assert!(gate.is_quiescent());
    }

    #[test]
    fn event_batch_is_bounded_and_reports_remaining_work() {
        let mut calls = 0usize;
        let batch = drain_event_batch(|| {
            calls += 1;
            crab_usb::Event::TransferActivity { count: 1 }
        });

        assert_eq!(calls, USBFS_EVENT_BATCH_LIMIT);
        assert_eq!(batch.transfer_events, USBFS_EVENT_BATCH_LIMIT);
        assert!(batch.exhausted);
    }

    #[test]
    fn shared_irq_reports_only_device_owned_interrupts_as_handled() {
        assert_eq!(
            usb_irq_return(false),
            ax_runtime::hal::irq::IrqReturn::Unhandled
        );
        assert_eq!(
            usb_irq_return(true),
            ax_runtime::hal::irq::IrqReturn::Handled
        );
    }

    #[test]
    fn event_permit_is_released_before_device_rearm() {
        let gate = UsbEventGate::new();
        gate.activate();
        let permit = match gate.try_enter() {
            UsbEventEntry::Acquired(permit) => permit,
            _ => panic!("an active USB event gate must issue its first permit"),
        };
        let mut rearmed = false;

        finish_event_drain(
            permit,
            false,
            || unreachable!(),
            || {
                let nested = gate.try_enter();
                assert!(
                    matches!(nested, UsbEventEntry::Acquired(_)),
                    "an immediately delivered IRQ must be able to acknowledge after rearm"
                );
                rearmed = true;
            },
        );

        assert!(rearmed);
    }

    #[test]
    fn partial_action_enable_rolls_back_previously_enabled_actions() {
        let mut disabled = Vec::new();

        let enabled = enable_actions_transactionally(
            [Some(1usize), Some(2), Some(3)],
            |handle| handle != 2,
            |handle| disabled.push(handle),
        );

        assert!(!enabled);
        assert_eq!(disabled, [1]);
    }
}
