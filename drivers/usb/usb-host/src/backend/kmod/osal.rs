use core::{ops::Deref, time::Duration};

use dma_api::{DeviceDma, DmaConstraints};

#[derive(Clone)]
pub(crate) struct Kernel {
    dma: DeviceDma,
    osal: &'static dyn KernelOp,
}

impl Kernel {
    pub fn new(dma: DeviceDma, osal: &'static dyn KernelOp) -> Self {
        Self { dma, osal }
    }

    pub fn delay(&self, duration: Duration) {
        self.osal.delay(duration)
    }
}

impl Deref for Kernel {
    type Target = DeviceDma;

    fn deref(&self) -> &Self::Target {
        &self.dma
    }
}

pub(crate) fn narrow_dma_capability(dma: &DeviceDma, hardware_mask: u64) -> DeviceDma {
    let current = dma.info().constraints();
    dma.with_constraints(DmaConstraints {
        addr_mask: current.addr_mask.min(hardware_mask),
        ..current
    })
}

pub trait KernelOp: Send + Sync {
    fn delay(&self, duration: Duration);
}

pub(crate) struct SpinWhile<F>
where
    F: Fn() -> bool,
{
    pub condition: F,
}

impl<F> SpinWhile<F>
where
    F: Fn() -> bool,
{
    #[must_use]
    pub fn new(condition: F) -> Self {
        Self { condition }
    }
}

impl<F> core::future::Future for SpinWhile<F>
where
    F: Fn() -> bool,
{
    type Output = ();

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        if (self.condition)() {
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        } else {
            core::task::Poll::Ready(())
        }
    }
}
