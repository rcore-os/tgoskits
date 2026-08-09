//! IRQ-safe publication of deferred vCPU kick work.
//!
//! This module deliberately carries no interrupt identity or pending state.
//! Architecture interrupt controllers own that state.  A hard-IRQ producer
//! publishes only the target-vCPU bit and wakes one pre-created worker.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_std::os::arceos::modules::ax_task::IrqNotify;

use crate::{AxVmResult, ax_err, sync::MutexExt};

const KICK_WORKER_STACK_SIZE: usize = 0x20_000;

/// Per-VM deferred vCPU kick publisher.
pub(crate) struct DeferredVcpuKick {
    vm_id: usize,
    pending_vcpus: AtomicUsize,
    worker_started: AtomicBool,
    stopping: AtomicBool,
    notify: IrqNotify,
    worker: Mutex<Option<crate::AxTaskRef>>,
}

impl DeferredVcpuKick {
    /// Creates an inactive publisher for one VM.
    pub(crate) fn new(vm_id: usize) -> Arc<Self> {
        Arc::new(Self {
            vm_id,
            pending_vcpus: AtomicUsize::new(0),
            worker_started: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            notify: IrqNotify::new(),
            worker: Mutex::new(None),
        })
    }

    /// Starts the task-context worker before an architecture enables IRQ input.
    pub(crate) fn start(self: &Arc<Self>) {
        let mut worker = self.worker.lock_unpoisoned();
        if worker.is_some() {
            return;
        }
        self.stopping.store(false, Ordering::Release);
        let state = self.clone();
        let task = crate::TaskInner::new(
            move || state.run_worker(),
            std::format!("VM[{}]-irq-kick", self.vm_id),
            KICK_WORKER_STACK_SIZE,
        );
        *worker = Some(crate::host::task::spawn_task(task));
        self.worker_started.store(true, Ordering::Release);
        if self.pending_vcpus.load(Ordering::Acquire) != 0 {
            self.notify.notify();
        }
    }

    /// Publishes that `vcpu_id` needs to observe controller-owned IRQ state.
    ///
    /// This is the only operation used by hard IRQ handlers.  It performs no
    /// allocation, VM lookup, runtime lookup, or ordinary-lock acquisition.
    pub(crate) fn publish_from_irq(&self, vcpu_id: usize) -> AxVmResult {
        let Some(bit) = 1usize.checked_shl(vcpu_id as u32) else {
            return ax_err!(
                InvalidInput,
                std::format!(
                    "VM[{}] vCPU {vcpu_id} exceeds the deferred IRQ kick bitmap",
                    self.vm_id
                )
            );
        };
        self.pending_vcpus.fetch_or(bit, Ordering::Release);
        if self.worker_started.load(Ordering::Acquire) {
            self.notify.notify_irq();
        }
        Ok(())
    }

    /// Stops and joins the worker after architecture IRQ input is quiesced.
    pub(crate) fn stop(&self) {
        self.worker_started.store(false, Ordering::Release);
        self.stopping.store(true, Ordering::Release);
        self.notify.notify();
        let worker = self.worker.lock_unpoisoned().take();
        if let Some(worker) = worker {
            worker.join();
        }
        self.pending_vcpus.store(0, Ordering::Release);
    }

    fn run_worker(&self) {
        loop {
            self.notify.wait();
            if self.stopping.load(Ordering::Acquire) {
                break;
            }
            let pending = self.pending_vcpus.swap(0, Ordering::AcqRel);
            for vcpu_id in SetBits(pending) {
                if let Err(error) = crate::runtime::vcpus::notify_vcpu(self.vm_id, vcpu_id) {
                    trace!(
                        "VM[{}] deferred IRQ kick for vCPU {vcpu_id} was not delivered: {error:?}",
                        self.vm_id
                    );
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn take_pending_for_test(&self) -> usize {
        self.pending_vcpus.swap(0, Ordering::AcqRel)
    }
}

struct SetBits(usize);

impl Iterator for SetBits {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0 == 0 {
            return None;
        }
        let bit = self.0.trailing_zeros() as usize;
        self.0 &= self.0 - 1;
        Some(bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_publication_coalesces_vcpu_bits_without_carrying_irq_state() {
        let kick = DeferredVcpuKick::new(7);

        kick.publish_from_irq(3).unwrap();
        kick.publish_from_irq(1).unwrap();
        kick.publish_from_irq(3).unwrap();

        assert_eq!(kick.take_pending_for_test(), (1 << 1) | (1 << 3));
        assert_eq!(kick.take_pending_for_test(), 0);
    }

    #[test]
    fn irq_publication_rejects_vcpus_outside_the_preallocated_bitmap() {
        let kick = DeferredVcpuKick::new(7);

        assert!(kick.publish_from_irq(usize::BITS as usize).is_err());
        assert_eq!(kick.take_pending_for_test(), 0);
    }
}
