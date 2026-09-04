//! VirtIO PCI common configuration transport.
//!
//! This module owns the transport state that is independent of a particular
//! PCI implementation: feature negotiation, queue configuration, device
//! configuration access, and queue notification.  The caller supplies guest
//! memory only while processing a queue notification.  In particular, merely
//! programming queue registers does not dereference guest memory.

use alloc::{format, sync::Arc};

use ax_sync::SpinLock;
use axdevice_base::{AccessWidth, DeviceError, DeviceResult};

use crate::{
    GuestMemory, NoGuestMemoryAccessor, VirtioDeviceID, VirtioError, VirtioQueue, map_virtio_error,
    pci::{InterruptTransition, VirtioPciInterruptCoordinator},
};

mod access;
mod queue;
mod reset;
mod state;
mod transition;

pub use state::ActivityPermit;
use state::{QueueActivity, QueueState, TransportState};
use transition::InterruptPublicationKind;
pub use transition::{
    InterruptPublicationRequest, InterruptTransitionIntent, InterruptTransitionRequest,
    QueueNotification, VirtioQueueGeneration,
};

pub(super) const COMMON_CONFIG_SIZE: u64 = 0x38;
pub(super) const NOTIFY_CONFIG_OFFSET: u64 = 0x100;
pub(super) const ISR_CONFIG_OFFSET: u64 = 0x200;
pub(super) const DEVICE_CONFIG_OFFSET: u64 = 0x300;
pub(super) const RESET_DRAIN_SPIN_LIMIT: usize = 1 << 20;

pub(super) const DEVICE_FEATURE_SELECT: u64 = 0x00;
pub(super) const DEVICE_FEATURE: u64 = 0x04;
pub(super) const DRIVER_FEATURE_SELECT: u64 = 0x08;
pub(super) const DRIVER_FEATURE: u64 = 0x0c;
pub(super) const MSIX_CONFIG: u64 = 0x10;
pub(super) const NUM_QUEUES: u64 = 0x12;
pub(super) const DEVICE_STATUS: u64 = 0x14;
pub(super) const CONFIG_GENERATION: u64 = 0x15;
pub(super) const QUEUE_SELECT: u64 = 0x16;
pub(super) const QUEUE_SIZE: u64 = 0x18;
pub(super) const QUEUE_MSIX_VECTOR: u64 = 0x1a;
pub(super) const QUEUE_ENABLE: u64 = 0x1c;
pub(super) const QUEUE_NOTIFY_OFF: u64 = 0x1e;
pub(super) const QUEUE_DESC: u64 = 0x20;
pub(super) const QUEUE_DRIVER: u64 = 0x28;
pub(super) const QUEUE_DEVICE: u64 = 0x30;

/// Result of a device-specific queue notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueNotifyOutcome {
    /// The device had no request to process.
    Idle,
    /// The device completed synchronously.
    Completed {
        /// Whether the used-ring update requires a guest interrupt.
        notify: bool,
    },
    /// The device handed work to an asynchronous backend.
    Deferred {
        /// Whether the eventual used-ring update requires a guest interrupt.
        notify: bool,
    },
}

/// Device-specific operations needed by the common VirtIO PCI transport.
///
/// The trait deliberately has no PCI or operating-system types beyond the
/// typed access width and guest-memory capability.  An endpoint adapter can
/// therefore expose this transport through any PCI resource and interrupt
/// implementation.
pub trait VirtioDeviceCore: Send + Sync {
    /// VirtIO device type advertised by the endpoint.
    fn device_type(&self) -> VirtioDeviceID;

    /// Feature bits offered by the endpoint.
    fn device_features(&self) -> u64;

    /// Maximum number of queues exposed by the endpoint.
    fn queue_num_max(&self) -> u16 {
        1
    }

    /// Maximum size of each queue.
    fn queue_size_max(&self) -> u16;

    /// Size of the device-specific configuration space.
    fn device_config_size(&self) -> u32;

    /// Read the device-specific configuration space.
    fn read_device_config(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64>;

    /// Write the device-specific configuration space.
    fn write_device_config(&self, offset: u64, width: AccessWidth, value: u64) -> DeviceResult;

    /// Process one queue notification using the caller's scoped memory grant.
    fn notify_queue(
        &self,
        queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome>;

    /// Whether queue processing can complete asynchronously.
    fn requires_deferred_processing(&self) -> bool {
        false
    }

    /// Reset device-specific state.
    fn reset(&self) -> DeviceResult {
        Ok(())
    }
}

/// Side effect produced by a common-config write.
pub enum VirtioPciWriteOutcome {
    /// No work is required by the endpoint adapter.
    None,
    /// The guest notified a queue and the core processed it.
    QueueNotified(QueueNotification),
    /// The transport was reset by writing zero to device status.
    Reset {
        /// Physical interrupt-line transition required after reset.
        interrupt: InterruptTransition,
    },
    /// Queue processing failed after admission and requires a device reset.
    Fault {
        /// The processing error returned to the guest-facing dispatcher.
        error: DeviceError,
        /// Config-change ISR publication retained through terminal IRQ handling.
        publication: InterruptPublicationRequest,
    },
}

/// Common VirtIO PCI transport state machine.
pub struct VirtioPciTransport<D: VirtioDeviceCore> {
    core: D,
    state: SpinLock<TransportState>,
    interrupts: Arc<VirtioPciInterruptCoordinator>,
    activity: Arc<QueueActivity>,
    device_config_size: u32,
    #[cfg(test)]
    notify_admission_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    reset_before_core_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl<D: VirtioDeviceCore> VirtioPciTransport<D> {
    /// Creates a transport with all queues disabled and unconfigured.
    ///
    /// # Errors
    ///
    /// Returns an error when the core does not expose exactly one queue, has
    /// a non-power-of-two queue size, or uses asynchronous processing. Commit
    /// 4 deliberately supports only the synchronous single-queue path; a
    /// multi-queue or asynchronous adapter belongs to a later integration.
    pub fn try_new(core: D) -> DeviceResult<Self> {
        let queue_num_max = core.queue_num_max();
        let queue_size_max = core.queue_size_max();
        if queue_num_max != 1 {
            return Err(DeviceError::InvalidInput {
                operation: "create VirtIO PCI transport",
                detail: "commit 4 supports exactly one queue".into(),
            });
        }
        if !queue_size_max.is_power_of_two() {
            return Err(DeviceError::InvalidInput {
                operation: "create VirtIO PCI transport",
                detail: "queue size must be a power of two".into(),
            });
        }
        if core.requires_deferred_processing() {
            return Err(DeviceError::Unsupported {
                operation: "create VirtIO PCI transport",
                detail: "deferred queue processing is not supported by the synchronous PCI adapter"
                    .into(),
            });
        }
        Ok(Self {
            device_config_size: core.device_config_size(),
            state: SpinLock::new(TransportState::new(queue_num_max, queue_size_max)),
            interrupts: Arc::new(VirtioPciInterruptCoordinator::new()),
            activity: Arc::new(QueueActivity::new()),
            core,
            #[cfg(test)]
            notify_admission_hook: SpinLock::new(None),
            #[cfg(test)]
            reset_before_core_hook: SpinLock::new(None),
        })
    }

    /// Returns the device-specific core.
    pub fn core(&self) -> &D {
        &self.core
    }

    /// Returns the device type advertised by the core.
    pub fn device_type(&self) -> VirtioDeviceID {
        self.core.device_type()
    }

    /// Returns the advertised device feature bits.
    pub fn device_features(&self) -> u64 {
        self.core.device_features()
    }

    /// Returns the current device status.
    pub fn status(&self) -> u8 {
        self.state.lock().status
    }

    /// Returns the negotiated driver feature bits.
    pub fn driver_features(&self) -> u64 {
        self.state.lock().driver_features
    }

    /// Returns the generation of the current queue configuration lifetime.
    pub fn queue_generation(&self) -> VirtioQueueGeneration {
        VirtioQueueGeneration(self.state.lock().queue_generation)
    }

    /// Returns whether the transport has a pending interrupt status bit.
    pub fn interrupt_pending(&self) -> bool {
        self.interrupts.pending()
    }

    /// Records a virtqueue or configuration interrupt in the PCI ISR state.
    #[cfg(test)]
    pub(crate) fn record_interrupt(&self, configuration_change: bool) -> InterruptTransition {
        if configuration_change {
            self.interrupts.record_config_change()
        } else {
            self.interrupts.record_queue_completion(true)
        }
    }

    /// Installs a test-only pause between queue snapshot and activity admission.
    ///
    /// This makes the reset/reconfiguration interleaving deterministic without
    /// exposing a production scheduling hook.
    #[cfg(test)]
    pub(crate) fn set_notify_admission_hook<F>(&self, hook: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self.notify_admission_hook.lock() = Some(Arc::new(hook));
    }

    #[cfg(test)]
    pub(super) fn run_notify_admission_hook(&self) {
        let hook = self.notify_admission_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Installs a test-only pause after activity has drained and before the
    /// device-specific reset begins. This makes reset handoff tests
    /// deterministic without exposing a production scheduling hook.
    #[cfg(test)]
    pub(crate) fn set_reset_before_core_hook<F>(&self, hook: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self.reset_before_core_hook.lock() = Some(Arc::new(hook));
    }

    #[cfg(test)]
    pub(super) fn run_reset_before_core_hook(&self) {
        let hook = self.reset_before_core_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Updates the logical PCI Command.INTx Disable state and returns the
    /// resulting physical-line transition intent.
    ///
    /// This operation is intentionally infallible and does not acquire
    /// activity or IRQ-transition admission. The endpoint adapter uses it
    /// while holding its per-function Command revision lock; the returned
    /// intent must be admitted and executed only after that lock is released.
    pub fn update_interrupt_disabled_logical(&self, disabled: bool) -> InterruptTransitionIntent {
        // Capture the queue generation before changing the coordinator.  If a
        // reset wins before activity admission, this intent must be rejected
        // rather than acquiring a permit from the reopened generation.
        let generation = self.queue_generation();
        let transition = self.interrupts.set_disabled(disabled);
        InterruptTransitionIntent::new(transition, generation)
    }

    /// Acquires control activity for a previously committed interrupt
    /// transition intent.
    ///
    /// Logical Command state is already committed when this method runs. If
    /// reset has closed activity admission, only the unexecuted physical
    /// intent is cancelled; the logical disabled state remains available for
    /// the next reset or synchronization point.
    pub fn admit_interrupt_transition(
        &self,
        intent: InterruptTransitionIntent,
    ) -> DeviceResult<Option<InterruptTransitionRequest>> {
        let Some(activity) = self.activity.acquire(intent.generation()) else {
            // The logical command state is committed before admission is
            // acquired. If reset closed admission, cancel only the
            // unexecuted physical intent; the desired state remains in
            // the coordinator for the reset owner to preserve/retry.
            self.interrupts.cancel_transition(intent.transition());
            return Ok(None);
        };

        // Activity admission and reset close are linearized by the same gate,
        // but the generation advances only when the reset core state is
        // committed. Revalidate after acquiring the permit so an intent that
        // waited through a completed reset cannot reach the endpoint IRQ
        // callback.
        if self.queue_generation() != intent.generation() {
            self.interrupts
                .suppress_stale_transition(intent.transition());
            drop(activity);
            return Ok(None);
        }

        Ok(Some(InterruptTransitionRequest::new(
            Arc::clone(&self.interrupts),
            intent.transition(),
            Some(activity),
        )))
    }

    /// Applies PCI Command.INTx Disable and returns a line transition intent.
    pub fn set_interrupt_disabled(
        &self,
        disabled: bool,
    ) -> DeviceResult<InterruptTransitionRequest> {
        let intent = self.update_interrupt_disabled_logical(disabled);
        self.admit_interrupt_transition(intent)?
            .ok_or(DeviceError::InvalidState {
                operation: "update VirtIO PCI interrupt state",
                detail: "transport reset is in progress or the transition is stale".into(),
            })
    }

    /// Commits an interrupt-line operation executed through the endpoint
    /// context without turning a host line failure into a guest access error.
    pub fn complete_interrupt_transition(
        &self,
        transition: InterruptTransition,
        success: bool,
    ) -> InterruptTransition {
        self.interrupts.complete_transition(transition, success)
    }

    /// Suppresses a transition whose endpoint IRQ admission became stale.
    ///
    /// Admission closure is not a physical-line failure. Only release the
    /// matching in-flight transition; preserve any retry state that was
    /// already recorded by a real line operation failure.
    pub fn suppress_stale_interrupt_transition(&self, transition: InterruptTransition) {
        self.interrupts.suppress_stale_transition(transition);
    }

    /// Returns a retry intent for a previously failed line synchronization.
    pub fn resynchronize_interrupt(&self) -> InterruptTransition {
        self.interrupts.resynchronize()
    }

    fn acquire_control_activity(&self) -> DeviceResult<ActivityPermit> {
        self.activity
            .acquire(self.queue_generation())
            .ok_or(DeviceError::InvalidState {
                operation: "access VirtIO PCI transport control state",
                detail: "transport reset is in progress".into(),
            })
    }
}

fn require_width(actual: AccessWidth, expected: AccessWidth) -> DeviceResult {
    if actual == expected {
        Ok(())
    } else {
        Err(DeviceError::InvalidWidth { expected, actual })
    }
}

fn access_in_region(offset: u64, width: AccessWidth, start: u64, length: u64) -> bool {
    offset >= start
        && offset
            .checked_add(width.size() as u64)
            .is_some_and(|end| end <= start + length)
}

fn feature_word(features: u64, selector: u32) -> DeviceResult<u64> {
    if selector > 1 {
        Ok(0)
    } else {
        Ok((features >> (selector * 32)) & u32::MAX as u64)
    }
}

fn invalid_queue(index: u16) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "virtio-pci queue",
        detail: format!("queue index {index} is not exposed"),
    }
}

fn map_pci_error(error: VirtioError) -> DeviceError {
    map_virtio_error(error, "virtio-pci queue")
}

fn reject_processing_queue(queue: &QueueState) -> DeviceResult {
    if queue.processing {
        Err(DeviceError::ResourceBusy {
            operation: "configure VirtIO queue",
            resource: "queue processing lease".into(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
