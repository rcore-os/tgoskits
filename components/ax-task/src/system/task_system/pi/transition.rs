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

/// Linux rq accounting path selected while changing one PI owner's class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PiOwnerRqAccountingPath {
    /// Linux's running path executes `put_prev_task()` for the PI owner.
    Running,
    /// Linux's queued path executes the owner's old class `dequeue_task()`.
    QueuedClassDequeue,
    /// The owner has neither a queued class entity nor the running context.
    Inactive,
}

/// Returns whether a PI owner update must settle the rq current interval.
#[inline]
pub(super) fn owner_rq_needs_current_settlement<C: Eq>(
    path: PiOwnerRqAccountingPath,
    owner_accounting_class: Option<C>,
    current_accounting_class: Option<C>,
) -> bool {
    match path {
        PiOwnerRqAccountingPath::Running => true,
        PiOwnerRqAccountingPath::QueuedClassDequeue => {
            owner_accounting_class.is_some() && owner_accounting_class == current_accounting_class
        }
        PiOwnerRqAccountingPath::Inactive => false,
    }
}
