/// Detaches a waiter edge before publishing the corresponding owner PI change.
///
/// The caller retains the physical mutex wait lock across all callbacks. A
/// failed owner publication restores the detached edge before returning.
#[inline]
pub(super) fn publish_owner_after_waiter_detach<S, D, T, E>(
    state: &mut S,
    detach_waiter: impl FnOnce(&mut S) -> Result<D, E>,
    publish_owner: impl FnOnce(&mut S, &D) -> Result<T, E>,
    restore_waiter: impl FnOnce(&mut S, D),
) -> Result<(D, T), E> {
    let detached = detach_waiter(state)?;
    match publish_owner(state, &detached) {
        Ok(published) => Ok((detached, published)),
        Err(error) => {
            restore_waiter(state, detached);
            Err(error)
        }
    }
}
