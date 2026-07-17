//! IRQ publication and acknowledgement ownership during controller recovery.

use core::sync::atomic::{Ordering, fence};

use ax_hal::irq::{IrqDrainWake, IrqError};
use rdif_block::{IdList, InitError, InitInput, InitSchedule};

use super::{BlockController, ControllerPhase};
use crate::irq::Registration;

pub(super) fn release_registration_quenches(
    registrations: &[Registration],
) -> Result<(), IrqError> {
    for registration in registrations {
        registration.release_quench()?;
    }
    Ok(())
}

impl BlockController {
    pub(super) fn finish_masked_source_continuations(&self) -> bool {
        self.irq_sources
            .iter()
            .all(|source| source.finish_after_device_masked())
    }

    pub(super) fn record_lifecycle_irq(&'static self, source_id: usize) -> bool {
        if source_id >= u64::BITS as usize || self.phase() != ControllerPhase::Recovering {
            return false;
        }
        let source = 1_u64 << source_id;
        let waiting = self.recovery_wait_sources.load(Ordering::Acquire);
        if waiting & source == 0 && !self.recovery_polling_irqs.load(Ordering::Acquire) {
            return false;
        }
        self.recovery_pending_sources
            .fetch_or(source, Ordering::Release);
        fence(Ordering::SeqCst);
        if self.recovery_wait_sources.load(Ordering::Acquire) & source != 0 {
            self.queue_recovery_work().is_ok()
        } else {
            // A worker currently inside poll_reinitialize will either consume
            // this bit directly or observe it after publishing its next wait
            // mask. It does not need a concurrent activation yet.
            false
        }
    }

    pub(super) fn irq_drain_wake(&'static self) -> &'static IrqDrainWake {
        &self.irq_drain_wake
    }

    /// Masks the device before disabling its OS acknowledgement actions.
    ///
    /// A `false` result means device masking or shared-line quench release was
    /// not proven, so the caller must retain the controller fail-closed.
    pub(super) fn mask_recovery_sources(&self) -> bool {
        if let Err(error) = self.device.lock().disable_irq() {
            error!(
                "block controller {} could not mask device IRQ delivery: {error}",
                self.name
            );
            return false;
        }
        if !self.finish_masked_source_continuations() {
            return false;
        }
        if let Some(registrations) = self.registrations.lock().as_ref() {
            if let Err(error) = release_registration_quenches(registrations) {
                error!(
                    "block controller {} could not release its quenched IRQ line: {error:?}",
                    self.name
                );
                return false;
            }
            for registration in registrations {
                let _ = registration.disable();
            }
        }
        self.recovery_irqs_enabled.store(false, Ordering::Release);
        true
    }

    pub(super) fn enable_recovery_irqs(&self) -> Result<(), InitError> {
        if self.recovery_irqs_enabled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.recovery_irq_drains.lock().fill(None);
        if let Some(registrations) = self.registrations.lock().as_ref() {
            for registration in registrations {
                if registration.enable().is_err() {
                    for registration in registrations {
                        let _ = registration.disable();
                    }
                    return Err(InitError::Hardware(
                        "could not enable a controller IRQ action",
                    ));
                }
            }
        }
        if let Err(error) = self.device.lock().enable_irq() {
            if let Err(mask_error) = self.device.lock().disable_irq() {
                // A fallible activation may have exposed the device source
                // before reporting failure. Keep every acknowledgement
                // action live until a later recovery pass proves the source
                // masked.
                error!(
                    "block controller {} could neither activate nor remask device IRQ delivery: \
                     enable={error}, remask={mask_error}",
                    self.name
                );
                return Err(InitError::Hardware(
                    "controller IRQ activation failed and the device could not be remasked",
                ));
            }
            if let Some(registrations) = self.registrations.lock().as_ref() {
                for registration in registrations {
                    let _ = registration.disable();
                }
            }
            error!(
                "block controller {} could not unmask device IRQs during reinitialization: {error}",
                self.name
            );
            return Err(InitError::Hardware(
                "could not unmask controller interrupt delivery",
            ));
        }
        self.recovery_irqs_enabled.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn recovery_input(&self, allow_irq: bool) -> InitInput {
        self.recovery_polling_irqs
            .store(allow_irq, Ordering::Release);
        self.recovery_wait_sources.store(0, Ordering::Release);
        fence(Ordering::SeqCst);
        InitInput::new(
            ax_hal::time::monotonic_time_nanos(),
            IdList::from_bits(self.recovery_pending_sources.swap(0, Ordering::AcqRel)),
        )
    }

    pub(super) fn finish_recovery_poll(&self) {
        self.recovery_wait_sources.store(0, Ordering::Release);
        self.recovery_polling_irqs.store(false, Ordering::Release);
    }

    pub(super) fn arm_recovery_schedule(
        &'static self,
        schedule: InitSchedule,
        allow_irq: bool,
    ) -> Result<(), InitError> {
        let schedule = schedule.validate()?;
        if !allow_irq && !schedule.irq_sources().is_empty() {
            self.finish_recovery_poll();
            return Err(InitError::InvalidState);
        }
        self.recovery_wait_sources
            .store(schedule.irq_sources().bits(), Ordering::Release);
        self.recovery_polling_irqs.store(false, Ordering::Release);
        fence(Ordering::SeqCst);
        let irq_ready = self.recovery_pending_sources.load(Ordering::Acquire)
            & schedule.irq_sources().bits()
            != 0;
        if !schedule.irq_sources().is_empty() {
            self.enable_recovery_irqs()?;
        }
        if schedule.run_again() || irq_ready {
            self.queue_recovery_work()
                .map_err(|_| InitError::Hardware("could not queue controller recovery work"))?;
        }
        if let Some(deadline_ns) = schedule.wake_at_ns() {
            let delay_ns = deadline_ns.saturating_sub(ax_hal::time::monotonic_time_nanos());
            self.recovery_domain
                .mod_delayed_work_on(self.recovery_domain.cpu(), self.recovery_timer(), delay_ns)
                .map_err(|_| InitError::Hardware("could not arm controller recovery deadline"))?;
        }
        Ok(())
    }
}
