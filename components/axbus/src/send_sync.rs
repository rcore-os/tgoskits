/// A wrapper that asserts `T` is `Send + Sync`.
///
/// Used by the legacy adapters to satisfy `VirtualDevice: Send + Sync` when
/// wrapping `Arc<dyn BaseDeviceOps<R>>` (which isn't automatically `Send + Sync`
/// because the trait lacks those bounds, but all concrete implementations are).
///
/// # Safety
///
/// The caller MUST ensure that `T` is actually safe to send and sync across
/// threads. In this codebase the only `T` is `Arc<dyn BaseDeviceOps<R>>` where
/// all concrete device implementations are `Send + Sync` by construction.
#[doc(hidden)]
pub struct AssertSendSync<T>(pub T);

impl<T> core::fmt::Debug for AssertSendSync<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AssertSendSync").finish_non_exhaustive()
    }
}

// SAFETY: The caller guarantees that the inner `T` is safe to send/sync across
// threads. This is true for all `BaseDeviceOps` implementations in AxVisor.
unsafe impl<T> Send for AssertSendSync<T> {}
unsafe impl<T> Sync for AssertSendSync<T> {}
