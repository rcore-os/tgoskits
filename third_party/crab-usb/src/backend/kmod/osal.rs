use core::ops::Deref;
use core::time::Duration;

use dma_api::DeviceDma;
pub use dma_api::{DmaAddr, DmaDirection, DmaError, DmaHandle, DmaMapHandle, DmaOp};

#[derive(Clone)]
pub(crate) struct Kernel {
    dma: DeviceDma,
    osal: &'static dyn KernelOp,
}

impl Kernel {
    pub fn new(dma_mask: u64, osal: &'static dyn KernelOp) -> Self {
        Self {
            dma: DeviceDma::new(dma_mask, osal),
            osal,
        }
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

pub trait KernelOp: DmaOp {
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
