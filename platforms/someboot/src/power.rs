use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    ArchTrait, DCacheOp,
    arch::Arch,
    mem::{__kimage_va, cpu_area_phys_to_virt, dcache_range, virt_to_phys},
    smp::{CpuBootPrepareError, CpuBootStatus, PerCpuMeta},
};

const NO_ACTIVE_SECONDARY_CPU: usize = usize::MAX;
static ACTIVE_SECONDARY_CPU: SecondaryCpuOwner = SecondaryCpuOwner::new();

pub fn shutdown() -> ! {
    crate::arch::Arch::shutdown()
}

pub fn reset() -> ! {
    crate::arch::Arch::reset()
}

/// Starts one secondary CPU without waiting for it to reach the common entry.
///
/// The returned handle is the unique owner of the in-flight startup. Call
/// [`SecondaryCpuStartup::status`] until it reports
/// [`SecondaryCpuStartupStatus::Alive`], then call
/// [`SecondaryCpuStartup::release`] to let the CPU enter the OS runtime.
///
/// Dropping an incomplete handle does not cancel the hardware request or
/// release the startup owner. A late secondary may still be executing through
/// a shared architecture trampoline, so another CPU cannot safely reuse that
/// transport state. There is deliberately no retry or cancellation operation.
///
/// # Errors
///
/// Returns [`CpuOnError::StartupInProgress`] if another secondary startup owns
/// the shared transport lifecycle. Architecture firmware and transport errors
/// are returned after the per-CPU state has been published as kicked; such an
/// error is terminal for this boot and keeps the startup owner claimed.
pub fn start_secondary_cpu(cpu_idx: usize) -> Result<SecondaryCpuStartup, CpuOnError> {
    if cpu_idx == NO_ACTIVE_SECONDARY_CPU {
        return Err(CpuOnError::InvalidParameters);
    }
    ACTIVE_SECONDARY_CPU
        .try_claim(cpu_idx)
        .map_err(|active_logical_cpu| CpuOnError::StartupInProgress {
            requested_logical_cpu: cpu_idx,
            active_logical_cpu,
        })?;

    let entry = secondary_entry_addr();
    debug!("Secondary entry address: {entry:#x}");
    let Some(arg) = crate::smp::cpu_meta_addr(cpu_idx) else {
        ACTIVE_SECONDARY_CPU
            .release(cpu_idx)
            .expect("the validating CPU must own the secondary startup");
        return Err(CpuOnError::InvalidParameters);
    };
    debug!("Secondary entry argument (cpu meta address): {arg:#x}");

    // SAFETY: cpu_meta_addr returns a published per-CPU metadata slot. The
    // object was constructed before runtime CPU-count publication and remains
    // immutable at a stable address until shutdown.
    let meta = unsafe { &*(cpu_area_phys_to_virt(arg) as *const PerCpuMeta) };
    debug!("Power on CPU {meta:#x?}");
    let kimg = crate::mem::kimage_range();
    let kimg_start = __kimage_va(kimg.start);
    let size = kimg.end - kimg.start;
    dcache_range(DCacheOp::Clean, kimg_start, size);

    if let Err(error) = crate::smp::prepare_secondary_boot(cpu_idx) {
        ACTIVE_SECONDARY_CPU
            .release(cpu_idx)
            .expect("the preparing CPU must own the secondary startup");
        return Err(prepare_error(cpu_idx, meta.cpu_id, error));
    }

    // Once the architecture request begins, failure cannot prove that the
    // target CPU did not consume shared trampoline state. Keep the owner
    // claimed on error so a later startup cannot overwrite it.
    Arch::kick_secondary_cpu(meta.cpu_id, entry, arg)?;
    Ok(SecondaryCpuStartup {
        logical_cpu: cpu_idx,
        hardware_id: meta.cpu_id,
    })
}

/// Unique ownership of one in-flight secondary CPU startup.
///
/// This handle is intentionally neither `Clone` nor `Copy`. Dropping it before
/// [`release`](Self::release) abandons the boot attempt without making shared
/// architecture startup state reusable.
#[derive(Debug)]
#[must_use = "an in-flight secondary CPU must be observed and explicitly released"]
pub struct SecondaryCpuStartup {
    logical_cpu: usize,
    hardware_id: usize,
}

impl SecondaryCpuStartup {
    /// Returns the dense logical CPU index owned by this startup.
    pub const fn logical_cpu(&self) -> usize {
        self.logical_cpu
    }

    /// Returns the firmware or hardware CPU ID targeted by this startup.
    pub const fn hardware_id(&self) -> usize {
        self.hardware_id
    }

    /// Observes whether the secondary has reached the common someboot entry.
    ///
    /// This query is side-effect free. In particular, observing
    /// [`SecondaryCpuStartupStatus::Alive`] does not release the CPU into the
    /// OS runtime.
    ///
    /// # Panics
    ///
    /// Panics if the per-CPU boot state no longer belongs to this live handle.
    /// That indicates an internal lifecycle invariant violation.
    pub fn status(&self) -> SecondaryCpuStartupStatus {
        match crate::smp::secondary_boot_status(self.logical_cpu).unwrap_or_else(|error| {
            panic!(
                "cannot observe logical CPU {} (hardware ID {:#x}): {error}",
                self.logical_cpu, self.hardware_id
            )
        }) {
            CpuBootStatus::WaitingForAlive => SecondaryCpuStartupStatus::WaitingForAlive,
            CpuBootStatus::Alive => SecondaryCpuStartupStatus::Alive,
        }
    }

    /// Releases an alive secondary CPU into the OS runtime.
    ///
    /// The handle is consumed so the startup owner can be released exactly
    /// once. Call [`status`](Self::status) first and release only after it
    /// reports [`SecondaryCpuStartupStatus::Alive`].
    ///
    /// # Errors
    ///
    /// Returns [`CpuOnError::NotAlive`] if the secondary has not reached the
    /// common entry. The startup remains claimed and cannot be retried.
    pub fn release(self) -> Result<(), CpuOnError> {
        ACTIVE_SECONDARY_CPU
            .ensure_owned(self.logical_cpu)
            .map_err(|active_logical_cpu| {
                CpuOnError::Other(anyhow::anyhow!(
                    "cannot release logical CPU {} (hardware ID {:#x}) while startup owner is \
                     {:#x}",
                    self.logical_cpu,
                    self.hardware_id,
                    active_logical_cpu
                ))
            })?;
        crate::smp::release_secondary_boot(self.logical_cpu).map_err(|error| match error {
            CpuBootPrepareError::UnexpectedState { .. } => CpuOnError::NotAlive {
                logical_cpu: self.logical_cpu,
                hardware_id: self.hardware_id,
            },
            CpuBootPrepareError::Missing { .. } => CpuOnError::InvalidParameters,
        })?;
        ACTIVE_SECONDARY_CPU
            .release(self.logical_cpu)
            .map_err(|active_logical_cpu| {
                CpuOnError::Other(anyhow::anyhow!(
                    "released logical CPU {} (hardware ID {:#x}) while startup owner was {:#x}",
                    self.logical_cpu,
                    self.hardware_id,
                    active_logical_cpu
                ))
            })
    }
}

/// Non-blocking observation of a secondary CPU startup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecondaryCpuStartupStatus {
    /// The architecture request was sent, but the CPU has not reached the
    /// common someboot entry.
    WaitingForAlive,
    /// The CPU reached the common someboot entry and is waiting for release.
    Alive,
}

fn prepare_error(logical_cpu: usize, hardware_id: usize, error: CpuBootPrepareError) -> CpuOnError {
    CpuOnError::Other(anyhow::anyhow!(
        "cannot kick logical CPU {logical_cpu} (hardware ID {hardware_id:#x}): {error}"
    ))
}

/// secondary entry address
/// arg0 is stack top
fn secondary_entry_addr() -> usize {
    let ptr = crate::arch::Arch::secondary_entry_fn_address() as *const u8;
    virt_to_phys(ptr)
}

struct SecondaryCpuOwner {
    active_cpu: AtomicUsize,
}

impl SecondaryCpuOwner {
    const fn new() -> Self {
        Self {
            active_cpu: AtomicUsize::new(NO_ACTIVE_SECONDARY_CPU),
        }
    }

    fn try_claim(&self, logical_cpu: usize) -> Result<(), usize> {
        self.active_cpu
            .compare_exchange(
                NO_ACTIVE_SECONDARY_CPU,
                logical_cpu,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
    }

    #[cfg(test)]
    fn active_cpu(&self) -> Option<usize> {
        match self.active_cpu.load(Ordering::Acquire) {
            NO_ACTIVE_SECONDARY_CPU => None,
            logical_cpu => Some(logical_cpu),
        }
    }

    fn release(&self, logical_cpu: usize) -> Result<(), usize> {
        self.active_cpu
            .compare_exchange(
                logical_cpu,
                NO_ACTIVE_SECONDARY_CPU,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
    }

    fn ensure_owned(&self, logical_cpu: usize) -> Result<(), usize> {
        let active_logical_cpu = self.active_cpu.load(Ordering::Acquire);
        if active_logical_cpu == logical_cpu {
            Ok(())
        } else {
            Err(active_logical_cpu)
        }
    }
}

/// Failure to start, observe, or release a secondary CPU.
#[derive(thiserror::Error, Debug)]
pub enum CpuOnError {
    /// The architecture or firmware does not implement secondary CPU startup.
    #[error("CPU on is not supported")]
    NotSupported,
    /// The target CPU is already running.
    #[error("CPU is already on")]
    AlreadyOn,
    /// The logical CPU index or architecture parameters are invalid.
    #[error("Invalid parameters")]
    InvalidParameters,
    /// Another CPU owns the serialized secondary startup lifecycle.
    #[error(
        "cannot start logical CPU {requested_logical_cpu}: logical CPU {active_logical_cpu} \
         already owns the secondary startup transport"
    )]
    StartupInProgress {
        requested_logical_cpu: usize,
        active_logical_cpu: usize,
    },
    /// Release was requested before the secondary reached the common entry.
    #[error(
        "logical CPU {logical_cpu} (hardware ID {hardware_id:#x}) has not reached the alive \
         synchronization point"
    )]
    NotAlive {
        logical_cpu: usize,
        hardware_id: usize,
    },
    /// The upper-layer wait policy expired before the secondary reported alive.
    #[error(
        "logical CPU {logical_cpu} (hardware ID {hardware_id:#x}) did not reach the alive \
         synchronization point"
    )]
    AliveTimeout {
        logical_cpu: usize,
        hardware_id: usize,
    },
    /// An architecture transport or internal lifecycle operation failed.
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_secondary_cpu_rejects_a_second_start_without_spinning() {
        let owner = SecondaryCpuOwner::new();

        assert_eq!(owner.try_claim(1), Ok(()));
        assert_eq!(owner.try_claim(2), Err(1));
    }

    #[test]
    fn active_secondary_cpu_is_released_only_explicitly() {
        let owner = SecondaryCpuOwner::new();

        owner.try_claim(1).unwrap();
        assert_eq!(owner.active_cpu(), Some(1));
        assert_eq!(owner.ensure_owned(2), Err(1));
        assert_eq!(owner.release(2), Err(1));
        assert_eq!(owner.active_cpu(), Some(1));

        owner.release(1).unwrap();
        assert_eq!(owner.active_cpu(), None);
        assert_eq!(owner.try_claim(2), Ok(()));
    }

    #[test]
    fn dropping_an_incomplete_handle_keeps_the_transport_claimed() {
        const LOGICAL_CPU: usize = 17;
        ACTIVE_SECONDARY_CPU.try_claim(LOGICAL_CPU).unwrap();

        let startup = SecondaryCpuStartup {
            logical_cpu: LOGICAL_CPU,
            hardware_id: 0x11,
        };
        drop(startup);

        assert_eq!(ACTIVE_SECONDARY_CPU.active_cpu(), Some(LOGICAL_CPU));
        assert_eq!(ACTIVE_SECONDARY_CPU.try_claim(18), Err(LOGICAL_CPU));
        ACTIVE_SECONDARY_CPU.release(LOGICAL_CPU).unwrap();
    }
}
