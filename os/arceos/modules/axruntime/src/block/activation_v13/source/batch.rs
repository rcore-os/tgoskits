//! Bounded vector transitions for move-only IRQ source owners.

use alloc::{boxed::Box, vec::Vec};
use core::{fmt, mem, num::NonZeroUsize};

use super::suspended::{QuiescedSourceReady, SourceRearmBatch};
use crate::maintenance::MaintenanceError;

/// Terminal-close owners selected before any action was rearmed.
#[must_use = "advance, retry, or quarantine every synchronized IRQ source owner"]
pub(in crate::block::activation_v13) struct SourceCloseBatch {
    pending: Vec<QuiescedSourceReady>,
    closed: usize,
}

impl SourceCloseBatch {
    pub(super) fn new(pending: Vec<QuiescedSourceReady>) -> Self {
        Self { pending, closed: 0 }
    }

    /// Closes at most `budget` synchronized actions.
    pub(in crate::block::activation_v13) fn advance(
        mut self,
        budget: NonZeroUsize,
    ) -> Result<SourceCloseBatchProgress, SourceCloseBatchFailure> {
        let attempt_count = core::cmp::min(budget.get(), self.pending.len());
        let mut unvisited = self.pending.split_off(attempt_count);
        let mut selected = mem::take(&mut self.pending).into_iter();

        while let Some(source) = selected.next() {
            match source.close_after_quiesce() {
                Ok(()) => self.closed += 1,
                Err(failure) => {
                    let (error, source) = failure.into_parts();
                    let mut pending = Vec::with_capacity(1 + selected.len() + unvisited.len());
                    pending.push(source);
                    pending.extend(selected);
                    pending.append(&mut unvisited);
                    self.pending = pending;
                    return Err(SourceCloseBatchFailure {
                        error,
                        batch: Box::new(self),
                    });
                }
            }
        }

        self.pending = unvisited;
        if self.pending.is_empty() {
            Ok(SourceCloseBatchProgress::Closed)
        } else {
            Ok(SourceCloseBatchProgress::More(self))
        }
    }

    pub(in crate::block::activation_v13) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub(in crate::block::activation_v13) const fn closed_len(&self) -> usize {
        self.closed
    }
}

/// Bounded terminal-close progress for synchronized source actions.
#[must_use = "continue closing or retain every remaining source owner"]
pub(in crate::block::activation_v13) enum SourceCloseBatchProgress {
    More(SourceCloseBatch),
    Closed,
}

/// Terminal-close failure retaining every source action not yet closed.
#[must_use = "retry close or quarantine the complete remaining source batch"]
pub(in crate::block::activation_v13) struct SourceCloseBatchFailure {
    error: MaintenanceError,
    batch: Box<SourceCloseBatch>,
}

impl fmt::Debug for SourceCloseBatchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceCloseBatchFailure")
            .field("error", &self.error)
            .field("pending", &self.batch.pending_len())
            .field("closed", &self.batch.closed_len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SourceCloseBatchFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "block IRQ source batch close failed: {}",
            self.error
        )
    }
}

impl core::error::Error for SourceCloseBatchFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Invalid attempt to choose terminal close after actions were rearmed.
#[must_use = "continue rearm or retain the original batch"]
pub(in crate::block::activation_v13) struct SourceTerminalChoiceFailure {
    pub(super) armed: usize,
    pub(super) batch: Box<SourceRearmBatch>,
}

impl fmt::Debug for SourceTerminalChoiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTerminalChoiceFailure")
            .field("armed", &self.armed)
            .field("pending", &self.batch.pending_len())
            .field("retained_armed", &self.batch.armed_len())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for SourceTerminalChoiceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "terminal close selected after {} block IRQ actions were rearmed",
            self.armed
        )
    }
}

impl core::error::Error for SourceTerminalChoiceFailure {}

/// Move-only owners in one bounded, retryable vector transition.
#[derive(Debug)]
pub(super) struct LinearOwnerBatch<I, O> {
    pub(super) pending: Vec<I>,
    pub(super) completed: Vec<O>,
}

impl<I, O> LinearOwnerBatch<I, O> {
    pub(super) fn new(pending: Vec<I>) -> Self {
        Self {
            pending,
            completed: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn from_parts(pending: Vec<I>, completed: Vec<O>) -> Self {
        Self { pending, completed }
    }

    pub(super) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn completed_len(&self) -> usize {
        self.completed.len()
    }

    #[cfg(test)]
    pub(super) fn into_parts(self) -> (Vec<I>, Vec<O>) {
        (self.pending, self.completed)
    }
}

/// One linear owner's result from a bounded transition attempt.
#[derive(Debug)]
pub(super) enum LinearOwnerTransition<I, O> {
    /// The same owner requires another bounded pass.
    Retained(I),
    /// The owner moved into the next typestate.
    Completed(O),
}

/// One failed owner transition retaining the exact input owner.
#[derive(Debug)]
pub(super) struct LinearOwnerTransitionFailure<I, E> {
    error: E,
    owner: Box<I>,
}

impl<I, E> LinearOwnerTransitionFailure<I, E> {
    pub(super) fn new(error: E, owner: I) -> Self {
        Self {
            error,
            owner: Box::new(owner),
        }
    }

    fn into_parts(self) -> (E, I) {
        (self.error, *self.owner)
    }
}

/// Bounded vector progress that never duplicates or discards an owner.
#[derive(Debug)]
pub(super) enum LinearOwnerBatchProgress<I, O> {
    /// Unvisited or retained owners remain for another pass.
    More(LinearOwnerBatch<I, O>),
    /// Every owner moved into the completed vector.
    Complete(Vec<O>),
}

/// Failed bounded vector transition retaining every input and output owner.
#[derive(Debug)]
pub(super) struct LinearOwnerBatchFailure<I, O, E> {
    error: E,
    batch: LinearOwnerBatch<I, O>,
}

impl<I, O, E> LinearOwnerBatchFailure<I, O, E> {
    pub(super) fn into_parts(self) -> (E, LinearOwnerBatch<I, O>) {
        (self.error, self.batch)
    }
}

/// Advances a move-only owner vector without losing partial progress on error.
pub(super) fn advance_linear_owner_batch<I, O, E>(
    mut batch: LinearOwnerBatch<I, O>,
    budget: NonZeroUsize,
    mut transition: impl FnMut(
        I,
    ) -> Result<
        LinearOwnerTransition<I, O>,
        LinearOwnerTransitionFailure<I, E>,
    >,
) -> Result<LinearOwnerBatchProgress<I, O>, LinearOwnerBatchFailure<I, O, E>> {
    let attempt_count = core::cmp::min(budget.get(), batch.pending.len());
    let mut unvisited = batch.pending.split_off(attempt_count);
    let mut selected = mem::take(&mut batch.pending).into_iter();
    let mut retained = Vec::new();

    while let Some(owner) = selected.next() {
        match transition(owner) {
            Ok(LinearOwnerTransition::Retained(owner)) => retained.push(owner),
            Ok(LinearOwnerTransition::Completed(owner)) => batch.completed.push(owner),
            Err(failure) => {
                let (error, failed_owner) = failure.into_parts();
                let mut pending =
                    Vec::with_capacity(1 + selected.len() + unvisited.len() + retained.len());
                pending.push(failed_owner);
                pending.extend(selected);
                pending.append(&mut unvisited);
                pending.append(&mut retained);
                batch.pending = pending;
                return Err(LinearOwnerBatchFailure { error, batch });
            }
        }
    }

    unvisited.append(&mut retained);
    batch.pending = unvisited;
    if batch.pending.is_empty() {
        Ok(LinearOwnerBatchProgress::Complete(batch.completed))
    } else {
        Ok(LinearOwnerBatchProgress::More(batch))
    }
}
