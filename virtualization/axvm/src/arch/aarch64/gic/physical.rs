//! IRQ-safe ingress for guest-owned physical GIC SPIs.
//!
//! The host top half acknowledges and priority-drops the interrupt before it
//! enters this module. It publishes through a preallocated route slot directly
//! into IRQ-safe canonical VGIC state; only the resulting vCPU kick is deferred.

use std::{
    boxed::Box,
    hint::spin_loop,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    },
    vec::Vec,
};

use arm_vgic::{AssignedSpiConfig, GicV3BackendError, GicV3Native, PhysicalIrqId};
use ax_std::os::arceos::sync::RawSpinLock;
use axdevice_base::HostIrqId;

use super::{deactivate_host_irq, dispatch_acknowledged_host_irq, host_irq_intid};

const GIC_INTID_COUNT: usize = 1020;

static ASSIGNED_SPI_ROUTES: [AssignedSpiRouteSlot; GIC_INTID_COUNT] =
    [const { AssignedSpiRouteSlot::new() }; GIC_INTID_COUNT];

/// Owns every fixed host-INTID route installed for one VM.
pub(crate) struct AssignedSpiRoutes {
    bindings: Box<[Arc<AssignedSpiBinding>]>,
    registrations: RawSpinLock<Vec<AssignedSpiRouteRegistration>>,
}

impl AssignedSpiRoutes {
    pub(super) fn register(
        controller: &GicV3Native,
        assigned_spis: &[AssignedSpiConfig],
    ) -> Result<Arc<Self>, GicV3BackendError> {
        let bindings = assigned_spis
            .iter()
            .map(|assigned| {
                Arc::new(AssignedSpiBinding {
                    irq: assigned.host_irq(),
                    controller: controller.clone(),
                    accepting: AtomicBool::new(false),
                    delivery: RawSpinLock::new(AssignedSpiDelivery::Idle),
                })
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let routes = Arc::new(Self {
            bindings,
            registrations: RawSpinLock::new(Vec::new()),
        });

        {
            let mut registrations = routes.registrations.lock_irqsave();
            for binding in &routes.bindings {
                match AssignedSpiRouteRegistration::install(binding) {
                    Ok(registration) => registrations.push(registration),
                    Err(error) => {
                        registrations.clear();
                        return Err(error);
                    }
                }
            }
        }
        for binding in &routes.bindings {
            binding.accepting.store(true, Ordering::Release);
        }
        Ok(routes)
    }

    /// Stops accepting new activations and drains task-context ingress.
    ///
    /// Route slots remain installed so a late acknowledged IRQ is consumed
    /// instead of escaping to a host driver after ownership has transferred.
    pub(crate) fn quiesce(&self) {
        for binding in &self.bindings {
            binding.accepting.store(false, Ordering::Release);
        }
        for binding in &self.bindings {
            binding.wait_for_publication();
        }
    }

    /// Restores ingress after a control-plane teardown attempt was rejected.
    pub(crate) fn resume(&self) {
        for binding in &self.bindings {
            binding.accepting.store(true, Ordering::Release);
        }
    }
}

impl Drop for AssignedSpiRoutes {
    fn drop(&mut self) {
        self.quiesce();
        self.registrations.lock_irqsave().clear();
    }
}

struct AssignedSpiBinding {
    irq: HostIrqId,
    controller: GicV3Native,
    accepting: AtomicBool,
    delivery: RawSpinLock<AssignedSpiDelivery>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssignedSpiDelivery {
    Idle,
    Active,
    Completing,
}

impl AssignedSpiBinding {
    /// Publishes one acknowledged activation without VM lookup or allocation.
    ///
    /// The ACK/DIR gate is held only across the canonical record step and, when
    /// that record fails, the matching rollback. The pre-bound vCPU wake (which
    /// publishes a deferred kick and may send an IPI) and every host-side failure
    /// callback run after the gate is released, so a raw guard never covers an
    /// external callback.
    fn publish_from_irq(&self, token: usize) -> bool {
        let spi = arm_vgic::SpiId::new(self.irq.value() as u32)
            .expect("assigned host IRQ is a validated SPI");
        // With a HW-backed LR, normal guest deactivation is performed by the
        // physical GIC and does not call the backend completion hook. A new
        // host acknowledgement is therefore the architectural proof that an
        // older `Active` marker has already retired and may be replaced.
        // Completion takes the same gate across DIR, so a level source cannot
        // publish a new activation into the old completion window. The record
        // and its rollback therefore share one gate acquisition: releasing the
        // gate between them would let a concurrent DIR completion consume a
        // half-published activation. Only the wake and the host callback below
        // run outside the gate.
        let (wake, failure, rejected) = {
            let mut delivery = self.delivery.lock_irqsave();
            if self.accepting.load(Ordering::Acquire) {
                *delivery = AssignedSpiDelivery::Active;
                match self.controller.acknowledge_physical_spi(spi) {
                    Ok(wake) => (wake, None, false),
                    Err(error) => {
                        // Roll back while the gate is still held, so a
                        // concurrent DIR completion cannot observe the
                        // half-published activation.
                        *delivery = AssignedSpiDelivery::Idle;
                        (None, Some(error), false)
                    }
                }
            } else {
                (None, None, true)
            }
        };
        // The gate is released: run every host-side callback and diagnostic here.
        if rejected || failure.is_some() {
            // Ownership already transferred, or the record rolled back: consume
            // the rejected host token outside the gate.
            deactivate_host_irq(token);
        }
        if let Some(error) = failure {
            warn!(
                "failed to forward assigned physical SPI {} into the VGIC: {error}",
                self.irq.value()
            );
        }
        // The canonical activation, when recorded, stays `Active` until its DIR
        // completion, so a failed kick is reported without touching the gate.
        if let Some(wake) = wake
            && let Err(error) = wake.wake()
        {
            warn!(
                "failed to kick the target vCPU for assigned physical SPI {}: {error}",
                self.irq.value()
            );
        }
        true
    }

    fn complete(
        &self,
        finish: impl FnOnce() -> Result<(), GicV3BackendError>,
    ) -> Result<bool, GicV3BackendError> {
        let mut delivery = self.delivery.lock_irqsave();
        if *delivery != AssignedSpiDelivery::Active {
            return Ok(false);
        }
        *delivery = AssignedSpiDelivery::Completing;
        match finish() {
            Ok(()) => {}
            Err(error) => {
                *delivery = AssignedSpiDelivery::Active;
                return Err(error);
            }
        };
        *delivery = AssignedSpiDelivery::Idle;
        drop(delivery);
        Ok(true)
    }

    fn wait_for_publication(&self) {
        drop(self.delivery.lock_irqsave());
    }
}

struct AssignedSpiRouteSlot {
    binding: AtomicPtr<AssignedSpiBinding>,
    readers: AtomicUsize,
}

impl AssignedSpiRouteSlot {
    const fn new() -> Self {
        Self {
            binding: AtomicPtr::new(ptr::null_mut()),
            readers: AtomicUsize::new(0),
        }
    }

    fn with_binding<R>(&self, operation: impl FnOnce(&AssignedSpiBinding) -> R) -> Option<R> {
        self.readers.fetch_add(1, Ordering::Acquire);
        let binding = self.binding.load(Ordering::Acquire);
        let result = if binding.is_null() {
            None
        } else {
            // SAFETY: removal clears the published pointer and waits for this
            // reader count before releasing the route-owned Arc.
            Some(operation(unsafe { &*binding }))
        };
        self.readers.fetch_sub(1, Ordering::Release);
        result
    }
}

struct AssignedSpiRouteRegistration {
    intid: usize,
    binding: usize,
}

impl AssignedSpiRouteRegistration {
    fn install(binding: &Arc<AssignedSpiBinding>) -> Result<Self, GicV3BackendError> {
        let intid = binding.irq.value();
        let Some(route) = ASSIGNED_SPI_ROUTES.get(intid) else {
            return Err(GicV3BackendError::value(
                "register assigned physical SPI route",
                "the host INTID is outside the assignable GIC range",
                intid as u64,
            ));
        };
        let raw = Arc::into_raw(binding.clone()) as *mut AssignedSpiBinding;
        if route
            .binding
            .compare_exchange(ptr::null_mut(), raw, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // SAFETY: the compare-exchange did not publish this strong
            // reference, so this call consumes exactly the reference above.
            drop(unsafe { Arc::from_raw(raw) });
            return Err(GicV3BackendError::value(
                "register assigned physical SPI route",
                "the host INTID is already assigned to another VM",
                intid as u64,
            ));
        }
        Ok(Self {
            intid,
            binding: raw as usize,
        })
    }
}

impl Drop for AssignedSpiRouteRegistration {
    fn drop(&mut self) {
        let route = &ASSIGNED_SPI_ROUTES[self.intid];
        let expected = self.binding as *mut AssignedSpiBinding;
        if route
            .binding
            .compare_exchange(
                expected,
                ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            warn!(
                "assigned physical SPI route {} changed before its owner released it",
                self.intid
            );
            return;
        }
        while route.readers.load(Ordering::Acquire) != 0 {
            spin_loop();
        }
        // SAFETY: installation transferred one strong reference to this
        // registration. The pointer is unpublished and all readers exited.
        drop(unsafe { Arc::from_raw(expected) });
    }
}

pub(super) fn route_acknowledged_host_irq(token: usize) -> Result<(), GicV3BackendError> {
    let intid = host_irq_intid(token) as usize;
    let published = ASSIGNED_SPI_ROUTES
        .get(intid)
        .and_then(|route| route.with_binding(|binding| binding.publish_from_irq(token)))
        .unwrap_or(false);
    if !published {
        dispatch_acknowledged_host_irq(token);
    }
    Ok(())
}

pub(super) fn complete_assigned_spi(
    irq: PhysicalIrqId,
    finish: impl FnOnce() -> Result<(), GicV3BackendError>,
) -> Result<Option<()>, GicV3BackendError> {
    let Some(route) = usize::try_from(irq.raw())
        .ok()
        .and_then(|intid| ASSIGNED_SPI_ROUTES.get(intid))
    else {
        return Ok(None);
    };
    route
        .with_binding(|binding| binding.complete(finish))
        .unwrap_or(Ok(false))
        .map(|completed| completed.then_some(()))
}
