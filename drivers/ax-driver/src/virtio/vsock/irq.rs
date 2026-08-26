//! Serialized transport access and IRQ/task ownership for VirtIO vsock.

use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::UnsafeCell,
    fmt::Debug,
    hint::spin_loop,
    ops::BitAnd,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_sync::PreemptIrqSaveGuard;
use bitflags::Flags;
use rdif_vsock::{
    VsockError, VsockHardIrqEndpoint, VsockHardIrqHandler, VsockHardIrqResult, VsockIrqEndpoints,
    VsockPollIrqControl, VsockRearmResult,
};
use virtio_drivers::{
    PhysAddr, Result as VirtIoResult,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

pub(super) struct SharedVsockTransport<T: Transport + 'static> {
    inner: Arc<VsockTransportCell<T>>,
}

impl<T: Transport + 'static> SharedVsockTransport<T> {
    pub(super) fn new(transport: T) -> (Self, VsockIrqEndpoints) {
        let inner = Arc::new(VsockTransportCell::new(transport));
        let endpoints = VsockIrqEndpoints::new(
            VsockHardIrqEndpoint::new(Box::new(VirtIoVsockHardIrq {
                transport: Arc::clone(&inner),
            })),
            Box::new(VirtIoVsockIrqControl {
                transport: Arc::clone(&inner),
            }),
        );
        (Self { inner }, endpoints)
    }
}

impl<T: Transport + 'static> Transport for SharedVsockTransport<T> {
    fn device_type(&self) -> DeviceType {
        self.inner.with_task(|transport| transport.device_type())
    }

    fn read_device_features(&mut self) -> u64 {
        self.inner
            .with_task(|transport| transport.read_device_features())
    }

    fn write_driver_features(&mut self, driver_features: u64) {
        self.inner
            .with_task(|transport| transport.write_driver_features(driver_features));
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

    fn set_guest_page_size(&mut self, guest_page_size: u32) {
        self.inner
            .with_task(|transport| transport.set_guest_page_size(guest_page_size));
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
        self.inner.with_task(|transport| transport.ack_interrupt())
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

    fn begin_init<F: Flags<Bits = u64> + BitAnd<Output = F> + Debug>(
        &mut self,
        supported_features: F,
    ) -> F {
        self.inner
            .with_task(|transport| transport.begin_init(supported_features))
    }

    fn finish_init(&mut self) {
        self.inner.with_task(Transport::finish_init);
    }
}

struct VsockTransportCell<T: Transport + 'static> {
    transport: UnsafeCell<T>,
    access_active: AtomicBool,
    irq_ack_pending: AtomicBool,
    irq_during_poll: AtomicBool,
    poll_active: AtomicBool,
    shutting_down: AtomicBool,
}

// SAFETY: every transport access is serialized by `access_active`; task-side
// access also disables local IRQs/preemption and IRQ context never waits.
unsafe impl<T: Transport + 'static> Send for VsockTransportCell<T> {}
// SAFETY: shared references only reach the transport through the same gate.
unsafe impl<T: Transport + 'static> Sync for VsockTransportCell<T> {}

impl<T: Transport + 'static> VsockTransportCell<T> {
    fn new(transport: T) -> Self {
        Self {
            transport: UnsafeCell::new(transport),
            access_active: AtomicBool::new(false),
            irq_ack_pending: AtomicBool::new(false),
            irq_during_poll: AtomicBool::new(false),
            poll_active: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
        }
    }

    fn with_task<R>(&self, operation: impl FnOnce(&mut T) -> R) -> R {
        let _irq_guard = PreemptIrqSaveGuard::new();
        let _access = VsockTransportAccessGuard::enter_task(&self.access_active);
        // SAFETY: the access guard serializes this mutable borrow with every
        // task and hard-IRQ transport access.
        let transport = unsafe { &mut *self.transport.get() };
        self.flush_deferred_irq_ack(transport);
        let result = operation(transport);
        self.flush_deferred_irq_ack(transport);
        result
    }

    fn try_with_irq<R>(&self, operation: impl FnOnce(&mut T) -> R) -> Option<R> {
        let _access = VsockTransportAccessGuard::try_enter_irq(&self.access_active)?;
        // SAFETY: IRQ context owns the non-blocking access gate for this call.
        Some(operation(unsafe { &mut *self.transport.get() }))
    }

    fn handle_irq(&self) -> VsockHardIrqResult {
        let Some(interrupt) = self.try_with_irq(Transport::ack_interrupt) else {
            self.irq_ack_pending.store(true, Ordering::Release);
            self.irq_during_poll.store(true, Ordering::Release);
            return VsockHardIrqResult::ProbeDeferred;
        };
        if interrupt.is_empty() {
            return VsockHardIrqResult::Spurious;
        }
        if self.shutting_down.load(Ordering::Acquire) {
            return VsockHardIrqResult::Handled;
        }
        self.irq_during_poll.store(true, Ordering::Release);
        if self.poll_active.load(Ordering::Acquire) {
            VsockHardIrqResult::Handled
        } else {
            VsockHardIrqResult::Schedule
        }
    }

    fn quiesce(&self) {
        self.poll_active.store(true, Ordering::Release);
        self.with_task(|transport| {
            let _ = transport.ack_interrupt();
        });
        // Interrupts acknowledged before the bounded drain are represented by
        // the queue contents the worker is about to consume.
        self.irq_during_poll.store(false, Ordering::Release);
    }

    fn rearm_and_check(&self) -> VsockRearmResult {
        if self.ack_or_observe_deferred_work() {
            return VsockRearmResult::WorkPending;
        }

        self.poll_active.store(false, Ordering::Release);
        if self.ack_or_observe_deferred_work() {
            self.poll_active.store(true, Ordering::Release);
            VsockRearmResult::WorkPending
        } else {
            VsockRearmResult::Idle
        }
    }

    fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.poll_active.store(true, Ordering::Release);
        self.with_task(|transport| {
            let _ = transport.ack_interrupt();
        });
    }

    fn ack_or_observe_deferred_work(&self) -> bool {
        let interrupt = self.with_task(Transport::ack_interrupt);
        !interrupt.is_empty() || self.irq_during_poll.swap(false, Ordering::AcqRel)
    }

    fn flush_deferred_irq_ack(&self, transport: &mut T) {
        if self.irq_ack_pending.swap(false, Ordering::AcqRel) {
            let _ = transport.ack_interrupt();
        }
    }
}

struct VsockTransportAccessGuard<'a>(&'a AtomicBool);

impl<'a> VsockTransportAccessGuard<'a> {
    fn enter_task(active: &'a AtomicBool) -> Self {
        while active
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Self(active)
    }

    fn try_enter_irq(active: &'a AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Self(active))
    }
}

impl Drop for VsockTransportAccessGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct VirtIoVsockHardIrq<T: Transport + 'static> {
    transport: Arc<VsockTransportCell<T>>,
}

impl<T: Transport + 'static> VsockHardIrqHandler for VirtIoVsockHardIrq<T> {
    fn handle_irq(&mut self) -> VsockHardIrqResult {
        self.transport.handle_irq()
    }
}

struct VirtIoVsockIrqControl<T: Transport + 'static> {
    transport: Arc<VsockTransportCell<T>>,
}

impl<T: Transport + 'static> VsockPollIrqControl for VirtIoVsockIrqControl<T> {
    fn quiesce(&mut self) -> Result<(), VsockError> {
        self.transport.quiesce();
        Ok(())
    }

    fn rearm_and_check(&mut self) -> Result<VsockRearmResult, VsockError> {
        Ok(self.transport.rearm_and_check())
    }

    fn shutdown(&mut self) -> Result<(), VsockError> {
        self.transport.shutdown();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use virtio_drivers::Error as VirtIoError;

    use super::*;

    #[derive(Default)]
    struct TestTransport {
        interrupt: InterruptStatus,
        status: DeviceStatus,
    }

    impl TestTransport {
        fn publish_queue_interrupt(&mut self) {
            self.interrupt |= InterruptStatus::QUEUE_INTERRUPT;
        }
    }

    impl Transport for TestTransport {
        fn device_type(&self) -> DeviceType {
            DeviceType::Socket
        }

        fn read_device_features(&mut self) -> u64 {
            0
        }

        fn write_driver_features(&mut self, _driver_features: u64) {}

        fn max_queue_size(&mut self, _queue: u16) -> u32 {
            256
        }

        fn notify(&mut self, _queue: u16) {}

        fn get_status(&self) -> DeviceStatus {
            self.status
        }

        fn set_status(&mut self, status: DeviceStatus) {
            self.status = status;
        }

        fn set_guest_page_size(&mut self, _guest_page_size: u32) {}

        fn requires_legacy_layout(&self) -> bool {
            false
        }

        fn queue_set(
            &mut self,
            _queue: u16,
            _size: u32,
            _descriptors: PhysAddr,
            _driver_area: PhysAddr,
            _device_area: PhysAddr,
        ) {
        }

        fn queue_unset(&mut self, _queue: u16) {}

        fn queue_used(&mut self, _queue: u16) -> bool {
            false
        }

        fn ack_interrupt(&mut self) -> InterruptStatus {
            core::mem::take(&mut self.interrupt)
        }

        fn read_config_generation(&self) -> u32 {
            0
        }

        fn read_config_space<U: FromBytes + IntoBytes>(&self, _offset: usize) -> VirtIoResult<U> {
            Err(VirtIoError::Unsupported)
        }

        fn write_config_space<U: IntoBytes + Immutable>(
            &mut self,
            _offset: usize,
            _value: U,
        ) -> VirtIoResult<()> {
            Err(VirtIoError::Unsupported)
        }
    }

    #[test]
    fn hard_irq_and_worker_recheck_close_every_wakeup_window() {
        let cell = VsockTransportCell::new(TestTransport::default());

        cell.with_task(TestTransport::publish_queue_interrupt);
        assert_eq!(cell.handle_irq(), VsockHardIrqResult::Schedule);
        cell.quiesce();
        assert_eq!(cell.rearm_and_check(), VsockRearmResult::Idle);

        cell.quiesce();
        cell.with_task(TestTransport::publish_queue_interrupt);
        assert_eq!(cell.handle_irq(), VsockHardIrqResult::Handled);
        assert_eq!(cell.rearm_and_check(), VsockRearmResult::WorkPending);
        assert_eq!(cell.rearm_and_check(), VsockRearmResult::Idle);

        cell.access_active.store(true, Ordering::Release);
        assert_eq!(cell.handle_irq(), VsockHardIrqResult::ProbeDeferred);
        cell.access_active.store(false, Ordering::Release);
        cell.quiesce();
        assert_eq!(cell.rearm_and_check(), VsockRearmResult::Idle);
    }
}
