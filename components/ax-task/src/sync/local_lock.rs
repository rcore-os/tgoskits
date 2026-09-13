//! CPU-local lock acquisition with migration exclusion before CPU selection.

use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};

use super::{MigrationGuard, SpinLock, SpinLockGuard};
use crate::{
    sched::{CpuId, cpu_topology_len},
    thread::TaskError,
};

/// One RT lock and protected value per possible CPU.
///
/// Acquisition first pins migration, then selects that CPU's lock. Tasks on
/// the same CPU may preempt one another and synchronize through PI contention.
pub struct LocalLock<T> {
    cpus: Vec<SpinLock<T>>,
}

/// Local lock ownership; releases the lock before the outer migration pin.
#[must_use]
pub struct LocalLockGuard<'a, T> {
    owner: SpinLockGuard<'a, T>,
    migration: MigrationGuard,
}

impl<T> LocalLock<T> {
    /// Initializes one protected value for every configured CPU.
    pub fn new(mut init: impl FnMut(CpuId) -> T) -> Result<Self, TaskError> {
        let cpus = (0..cpu_topology_len()?)
            .map(|cpu| SpinLock::new(init(CpuId::new(cpu as u32))))
            .collect();
        Ok(Self { cpus })
    }

    /// Acquires the calling task's CPU-local value in preemptible task context.
    pub fn lock(&self) -> LocalLockGuard<'_, T> {
        let migration = MigrationGuard::new().expect("local lock requires task context");
        let owner = self.cpus[migration.cpu().as_usize()].lock();
        LocalLockGuard { owner, migration }
    }

    /// Attempts acquisition without sleeping.
    pub fn try_lock(&self) -> Option<LocalLockGuard<'_, T>> {
        let migration = MigrationGuard::new().ok()?;
        let owner = self.cpus[migration.cpu().as_usize()].try_lock()?;
        Some(LocalLockGuard { owner, migration })
    }

    /// RT IRQ-save spelling; hardware IRQ state is unchanged.
    pub fn lock_irqsave(&self) -> LocalLockGuard<'_, T> {
        self.lock()
    }
    /// RT IRQ-save spelling; hardware IRQ state is unchanged.
    pub fn try_lock_irqsave(&self) -> Option<LocalLockGuard<'_, T>> {
        self.try_lock()
    }
}

impl<T> LocalLockGuard<'_, T> {
    /// The CPU whose value remains protected across preemption.
    pub fn cpu(&self) -> CpuId {
        self.migration.cpu()
    }
}
impl<T> Deref for LocalLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.owner
    }
}
impl<T> DerefMut for LocalLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.owner
    }
}
