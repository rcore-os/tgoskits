//! Deferred physical PLIC claims backing guest-owned vPLIC sources.

use std::{
    boxed::Box,
    hint::spin_loop,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering},
    },
    vec::Vec,
};

use ax_std::os::arceos::{modules::ax_task::IrqNotify, sync::IrqSafeMutex};
use axvm_types::InterruptTriggerMode;
use riscv_vplic::{PLIC_NUM_SOURCES, VPlicGlobal};

use super::DeferredVcpuKick;
use crate::{AxTaskRef, AxVmError, AxVmResult, TaskInner, ax_err};

const PHYSICAL_IRQ_WORKER_STACK_SIZE: usize = 0x20_000;
const CLAIM_IDLE: u8 = 0;
const CLAIM_INGRESS: u8 = 1;
const CLAIM_ACTIVE: u8 = 2;

static PHYSICAL_PLIC_ROUTES: [PhysicalRouteSlot; PLIC_NUM_SOURCES] =
    [const { PhysicalRouteSlot::new() }; PLIC_NUM_SOURCES];

pub(super) fn publish_physical_claim_from_irq(source: u32) -> bool {
    let source = source as usize;
    if source == 0 || source >= PLIC_NUM_SOURCES {
        return false;
    }
    PHYSICAL_PLIC_ROUTES[source].with_binding(PhysicalSourceBinding::publish_from_irq)
}

pub(super) struct PhysicalIrqBridge {
    shared: Arc<PhysicalBridgeShared>,
    bindings: Box<[Arc<PhysicalSourceBinding>]>,
    registrations: IrqSafeMutex<Vec<PhysicalRouteRegistration>>,
    worker: IrqSafeMutex<Option<AxTaskRef>>,
    running: AtomicBool,
}

impl PhysicalIrqBridge {
    pub(super) fn new(
        vm_id: usize,
        vplic: Arc<VPlicGlobal>,
        kick: Arc<DeferredVcpuKick>,
        vcpu_count: usize,
        routes: &[crate::config::PassthroughInterrupt],
        target_cpu: usize,
    ) -> AxVmResult<Arc<Self>> {
        let shared = Arc::new(PhysicalBridgeShared {
            vm_id,
            vplic,
            kick,
            vcpu_count,
            notify: IrqNotify::new(),
            stopping: AtomicBool::new(false),
        });
        let mut bindings = Vec::with_capacity(routes.len());
        for route in routes {
            let source = route.source as usize;
            if source == 0 || source >= PLIC_NUM_SOURCES {
                return ax_err!(
                    InvalidInput,
                    std::format!(
                        "RISC-V physical PLIC source {} is outside 1..{PLIC_NUM_SOURCES}",
                        route.source
                    )
                );
            }
            if bindings
                .iter()
                .any(|binding: &Arc<PhysicalSourceBinding>| binding.source == source)
            {
                return ax_err!(
                    AlreadyExists,
                    std::format!("RISC-V physical PLIC source {source} is configured twice")
                );
            }
            bindings.push(Arc::new(PhysicalSourceBinding {
                source,
                trigger: route.trigger,
                target_cpu,
                shared: shared.clone(),
                accepting: AtomicBool::new(false),
                claim_state: AtomicU8::new(CLAIM_IDLE),
            }));
        }
        Ok(Arc::new(Self {
            shared,
            bindings: bindings.into_boxed_slice(),
            registrations: IrqSafeMutex::new(Vec::new()),
            worker: IrqSafeMutex::new(None),
            running: AtomicBool::new(false),
        }))
    }

    pub(super) fn start(self: &Arc<Self>) -> AxVmResult {
        if self.running.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.start_worker();
        if let Err(error) = self.install_and_activate_routes() {
            self.rollback_start();
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn stop(&self) -> AxVmResult {
        if !self.running.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        for binding in &self.bindings {
            binding.accepting.store(false, Ordering::Release);
        }

        let mut first_error = None;
        for binding in &self.bindings {
            if let Err(error) =
                ax_plat::irq::riscv64_hv::deactivate_guest_plic_source(binding.source as u32)
                && first_error.is_none()
            {
                first_error = Some(AxVmError::interrupt(
                    "deactivate RISC-V guest PLIC source",
                    std::format!("{error:?}"),
                ));
            }
        }
        self.registrations.lock().clear();
        self.stop_worker();
        self.release_outstanding_claims();

        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn complete_source(&self, source: usize) {
        let Some(binding) = self
            .bindings
            .iter()
            .find(|binding| binding.source == source)
        else {
            return;
        };
        if binding
            .claim_state
            .compare_exchange(
                CLAIM_ACTIVE,
                CLAIM_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        binding.lower_virtual_input();
        if !ax_plat::irq::riscv64_hv::complete_guest_plic_source(source as u32) {
            warn!(
                "VM[{}] completed physical PLIC source {source} without a retained host claim",
                self.shared.vm_id
            );
        }
    }

    fn start_worker(self: &Arc<Self>) {
        self.shared.stopping.store(false, Ordering::Release);
        let bridge = self.clone();
        let task = TaskInner::new(
            move || bridge.run_worker(),
            std::format!("VM[{}]-plic-physical", self.shared.vm_id),
            PHYSICAL_IRQ_WORKER_STACK_SIZE,
        );
        *self.worker.lock() = Some(crate::host::task::spawn_task(task));
    }

    fn install_and_activate_routes(&self) -> AxVmResult {
        {
            let mut registrations = self.registrations.lock();
            for binding in &self.bindings {
                registrations.push(PhysicalRouteRegistration::install(binding)?);
            }
        }

        for binding in &self.bindings {
            binding.accepting.store(true, Ordering::Release);
            if let Err(error) = ax_plat::irq::riscv64_hv::activate_guest_plic_source(
                binding.source as u32,
                binding.target_cpu,
            ) {
                binding.accepting.store(false, Ordering::Release);
                return Err(AxVmError::interrupt(
                    "activate RISC-V guest PLIC source",
                    std::format!("{error:?}"),
                ));
            }
        }
        Ok(())
    }

    fn rollback_start(&self) {
        for binding in &self.bindings {
            binding.accepting.store(false, Ordering::Release);
            let _ = ax_plat::irq::riscv64_hv::deactivate_guest_plic_source(binding.source as u32);
        }
        self.registrations.lock().clear();
        self.stop_worker();
        self.release_outstanding_claims();
        self.running.store(false, Ordering::Release);
    }

    fn stop_worker(&self) {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.notify.notify();
        if let Some(worker) = self.worker.lock().take() {
            worker.join();
        }
    }

    fn run_worker(&self) {
        loop {
            self.shared.notify.wait();
            if self.shared.stopping.load(Ordering::Acquire) {
                break;
            }
            for binding in &self.bindings {
                binding.drain_ingress();
            }
        }
    }

    fn release_outstanding_claims(&self) {
        for binding in &self.bindings {
            let previous = binding.claim_state.swap(CLAIM_IDLE, Ordering::AcqRel);
            if previous == CLAIM_IDLE {
                continue;
            }
            binding.clear_virtual_input();
            if !ax_plat::irq::riscv64_hv::complete_guest_plic_source(binding.source as u32) {
                trace!(
                    "VM[{}] found no retained host claim while releasing PLIC source {}",
                    self.shared.vm_id, binding.source
                );
            }
        }
    }

    #[cfg(test)]
    pub(super) fn drain_ingress_for_test(&self) {
        for binding in &self.bindings {
            binding.drain_ingress();
        }
    }
}

impl Drop for PhysicalIrqBridge {
    fn drop(&mut self) {
        if self.running.load(Ordering::Acquire) {
            warn!(
                "VM[{}] physical PLIC bridge dropped while active",
                self.shared.vm_id
            );
        }
    }
}

struct PhysicalBridgeShared {
    vm_id: usize,
    vplic: Arc<VPlicGlobal>,
    kick: Arc<DeferredVcpuKick>,
    vcpu_count: usize,
    notify: IrqNotify,
    stopping: AtomicBool,
}

struct PhysicalSourceBinding {
    source: usize,
    trigger: InterruptTriggerMode,
    target_cpu: usize,
    shared: Arc<PhysicalBridgeShared>,
    accepting: AtomicBool,
    claim_state: AtomicU8,
}

impl PhysicalSourceBinding {
    fn publish_from_irq(&self) -> bool {
        if !self.accepting.load(Ordering::Acquire)
            || self
                .claim_state
                .compare_exchange(
                    CLAIM_IDLE,
                    CLAIM_INGRESS,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return false;
        }
        self.shared.notify.notify_irq();
        true
    }

    fn drain_ingress(&self) {
        if self
            .claim_state
            .compare_exchange(
                CLAIM_INGRESS,
                CLAIM_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let inject_result = match self.trigger {
            InterruptTriggerMode::EdgeTriggered => self.shared.vplic.set_pending(self.source),
            InterruptTriggerMode::LevelTriggered => self
                .shared
                .vplic
                .set_irq_line_level(self.source, true)
                .map(|_| ()),
        };
        if let Err(error) = inject_result {
            self.claim_state.store(CLAIM_IDLE, Ordering::Release);
            let _ = ax_plat::irq::riscv64_hv::complete_guest_plic_source(self.source as u32);
            warn!(
                "VM[{}] failed to publish physical PLIC source {} into vPLIC: {error}",
                self.shared.vm_id, self.source
            );
            return;
        }
        for vcpu_id in 0..self.shared.vcpu_count {
            if let Err(error) = self.shared.kick.publish_from_irq(vcpu_id) {
                warn!(
                    "VM[{}] failed to publish physical PLIC source {} wake for vCPU {vcpu_id}: \
                     {error:?}",
                    self.shared.vm_id, self.source
                );
            }
        }
    }

    fn lower_virtual_input(&self) {
        if self.trigger == InterruptTriggerMode::LevelTriggered
            && let Err(error) = self.shared.vplic.set_irq_line_level(self.source, false)
        {
            warn!(
                "VM[{}] failed to lower completed physical PLIC source {}: {error}",
                self.shared.vm_id, self.source
            );
        }
    }

    fn clear_virtual_input(&self) {
        let result = match self.trigger {
            InterruptTriggerMode::EdgeTriggered => self.shared.vplic.clear_pending(self.source),
            InterruptTriggerMode::LevelTriggered => self
                .shared
                .vplic
                .set_irq_line_level(self.source, false)
                .map(|_| ()),
        };
        if let Err(error) = result {
            warn!(
                "VM[{}] failed to clear physical PLIC source {} during teardown: {error}",
                self.shared.vm_id, self.source
            );
        }
    }
}

struct PhysicalRouteSlot {
    binding: AtomicPtr<PhysicalSourceBinding>,
    readers: AtomicUsize,
}

impl PhysicalRouteSlot {
    const fn new() -> Self {
        Self {
            binding: AtomicPtr::new(ptr::null_mut()),
            readers: AtomicUsize::new(0),
        }
    }

    fn with_binding(&self, publish: impl FnOnce(&PhysicalSourceBinding) -> bool) -> bool {
        self.readers.fetch_add(1, Ordering::Acquire);
        let binding = self.binding.load(Ordering::Acquire);
        let published = if binding.is_null() {
            false
        } else {
            // SAFETY: route removal first clears `binding`, then waits for this
            // reader count to reach zero before dropping the route-owned Arc.
            publish(unsafe { &*binding })
        };
        self.readers.fetch_sub(1, Ordering::Release);
        published
    }
}

struct PhysicalRouteRegistration {
    source: usize,
    binding: usize,
}

impl PhysicalRouteRegistration {
    fn install(binding: &Arc<PhysicalSourceBinding>) -> AxVmResult<Self> {
        let source = binding.source;
        let raw = Arc::into_raw(binding.clone()) as *mut PhysicalSourceBinding;
        if PHYSICAL_PLIC_ROUTES[source]
            .binding
            .compare_exchange(ptr::null_mut(), raw, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // SAFETY: this raw pointer came from `Arc::into_raw` above and was
            // not published because the compare-exchange failed.
            drop(unsafe { Arc::from_raw(raw) });
            return Err(AxVmError::resource_conflict(
                "RISC-V physical PLIC route",
                std::format!("source {source} is already assigned to another VM"),
            ));
        }
        Ok(Self {
            source,
            binding: raw as usize,
        })
    }
}

impl Drop for PhysicalRouteRegistration {
    fn drop(&mut self) {
        let route = &PHYSICAL_PLIC_ROUTES[self.source];
        let expected = self.binding as *mut PhysicalSourceBinding;
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
                "RISC-V physical PLIC route {} changed before its owner released it",
                self.source
            );
            return;
        }
        while route.readers.load(Ordering::Acquire) != 0 {
            spin_loop();
        }
        // SAFETY: installation transferred exactly one strong reference into
        // this route. The pointer is now unpublished and all readers exited.
        drop(unsafe { Arc::from_raw(expected) });
    }
}

#[cfg(test)]
mod tests {
    use axvm_types::GuestPhysAddr;

    use super::*;

    fn bridge(trigger: InterruptTriggerMode) -> Arc<PhysicalIrqBridge> {
        let vplic = Arc::new(
            VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x60_0000), 2).unwrap(),
        );
        PhysicalIrqBridge::new(
            7,
            vplic,
            DeferredVcpuKick::new(7),
            1,
            &[crate::config::PassthroughInterrupt { source: 8, trigger }],
            0,
        )
        .unwrap()
    }

    #[test]
    fn hard_irq_publication_only_sets_preallocated_ingress_state() {
        let bridge = bridge(InterruptTriggerMode::EdgeTriggered);
        let binding = &bridge.bindings[0];
        binding.accepting.store(true, Ordering::Release);

        assert!(binding.publish_from_irq());
        assert!(!bridge.shared.vplic.is_pending(8).unwrap());
        assert_eq!(binding.claim_state.load(Ordering::Acquire), CLAIM_INGRESS);

        bridge.drain_ingress_for_test();
        assert!(bridge.shared.vplic.is_pending(8).unwrap());
        assert_eq!(binding.claim_state.load(Ordering::Acquire), CLAIM_ACTIVE);
    }

    #[test]
    fn route_slot_waits_for_readers_and_releases_the_exact_binding() {
        let bridge = bridge(InterruptTriggerMode::LevelTriggered);
        let binding = bridge.bindings[0].clone();
        binding.accepting.store(true, Ordering::Release);
        let registration = PhysicalRouteRegistration::install(&binding).unwrap();

        assert!(PHYSICAL_PLIC_ROUTES[8].with_binding(PhysicalSourceBinding::publish_from_irq));
        drop(registration);
        assert!(!PHYSICAL_PLIC_ROUTES[8].with_binding(PhysicalSourceBinding::publish_from_irq));
    }
}
