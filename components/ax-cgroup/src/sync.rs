//! Synchronization selected by the consuming execution environment.

#[cfg(not(test))]
pub(crate) use ax_sync::SpinLock as CgroupMutex;

#[cfg(test)]
pub(crate) struct CgroupMutex<T>(std::sync::Mutex<T>);

#[cfg(test)]
impl<T> CgroupMutex<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }

    pub(crate) fn lock_irqsave(&self) -> std::sync::MutexGuard<'_, T> {
        self.0.lock().expect("cgroup test mutex poisoned")
    }

    pub(crate) fn lock_irqsave_nested(&self, _subclass: u32) -> std::sync::MutexGuard<'_, T> {
        self.lock_irqsave()
    }
}
