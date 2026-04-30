use crate::mutex::RawMutex;

pub(crate) struct Lockdep {
    inner: ax_lockdep::Lockdep,
}

impl Lockdep {
    #[inline(always)]
    pub(crate) fn prepare(lock: &RawMutex, is_try: bool) -> Self {
        let addr = lock as *const _ as *const () as usize;
        Self {
            inner: ax_lockdep::Lockdep::prepare("mutex", addr, is_try, None),
        }
    }

    #[inline(always)]
    pub(crate) fn finish(self, acquired: bool) {
        self.inner.finish(acquired);
    }
}

#[inline(always)]
pub(crate) fn release(lock: &RawMutex) {
    let addr = lock as *const _ as *const () as usize;
    ax_lockdep::Lockdep::release("mutex", addr, None);
}
