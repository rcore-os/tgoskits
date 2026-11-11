//! Typed backend work survives the end of namespace exclusion.

use alloc::{boxed::Box, vec::Vec};

use super::{NamespaceEffect, NamespaceOutcome};
use crate::{
    DirEntry, FilesystemOps, NamespaceAdmissions, VfsResult, namespace_admission::owner_address,
};

/// Backend-owned typed pressure/abort state. This object holds no backend gate,
/// slot, authorization or name publication scope. It must not reenter admission.
pub trait NamespacePressure<T = NamespaceEffect> {
    /// Consumes phase two and returns its new error separately from the original
    /// operation result. Do not merge this phase into `outcome` here: an outer
    /// scope must preserve error order across every pressure and finish.
    /// Retry classification must already be visible at the backend handoff.
    fn pressure(self: Box<Self>, outcome: &mut NamespaceOutcome<T>) -> NamespacePressureResult;
}

/// One pressure phase's result and admission-independent remaining ownership.
///
/// The result reports this phase, not an error repeated from the original
/// attempt. Owners needed after admission exit must survive in `finish`, even
/// when pressure fails. Construction neither runs nor merges either phase.
pub struct NamespacePressureResult {
    completion: VfsResult<()>,
    finish: Option<Box<dyn NamespaceFinish>>,
}

impl NamespacePressureResult {
    pub fn new(completion: VfsResult<()>, finish: Option<Box<dyn NamespaceFinish>>) -> Self {
        Self { completion, finish }
    }
}

/// Reap and directory-sync owners, used only after *all* admissions are gone.
/// Dropping this object does not explicitly reap, sync or roll back.
pub trait NamespaceFinish {
    fn finish(self: Box<Self>) -> VfsResult<()>;
}

/// No-lock completion, including attempts that never produced an observation.
#[must_use = "finish pressure/abort before releasing all operation admissions"]
pub struct NamespaceCompletion<T = NamespaceEffect> {
    outcome: NamespaceOutcome<T>,
    owner: Option<usize>,
    pressure: Option<Box<dyn NamespacePressure<T>>>,
    parents: Vec<DirEntry>,
}

impl<T> NamespaceCompletion<T> {
    /// Backend completion payload; the fixed request binds the actual owner.
    pub fn new(
        outcome: NamespaceOutcome<T>,
        pressure: Option<Box<dyn NamespacePressure<T>>>,
    ) -> Self {
        Self {
            outcome,
            owner: None,
            pressure,
            parents: Vec::new(),
        }
    }

    pub fn outcome(&self) -> &NamespaceOutcome<T> {
        &self.outcome
    }

    pub(crate) fn bind(mut self, owner: &dyn FilesystemOps) -> Self {
        self.owner = Some(owner_address(owner));
        self
    }

    pub(super) fn retain_parents(mut self, parents: (&DirEntry, &DirEntry)) -> Self {
        self.parents.extend([parents.0.clone(), parents.1.clone()]);
        self
    }

    pub(crate) fn admitted_by(&self, admissions: &NamespaceAdmissions<'_>) -> bool {
        self.owner
            .is_some_and(|owner| admissions.contains_owner(owner))
    }

    pub(crate) fn pressure(self) -> NamespaceDeferred<T> {
        let (mut deferred, completion) = self.pressure_step();
        deferred.outcome = deferred.outcome.with_completion(completion);
        deferred
    }

    /// Preserves the phase result before a composite owner chooses error order.
    pub(crate) fn pressure_step(mut self) -> (NamespaceDeferred<T>, VfsResult<()>) {
        let phase = self.pressure.take().map_or_else(
            || NamespacePressureResult::new(Ok(()), None),
            |pressure| pressure.pressure(&mut self.outcome),
        );
        let completion = phase.completion;
        (
            NamespaceDeferred {
                outcome: self.outcome,
                finish: phase.finish,
                _parents: self.parents,
            },
            completion,
        )
    }
}

/// Phase-four work. No admission, gate or borrowed mutation input is retained.
#[must_use = "release every FS admission and retired owner before explicit completion"]
pub struct NamespaceDeferred<T = NamespaceEffect> {
    outcome: NamespaceOutcome<T>,
    finish: Option<Box<dyn NamespaceFinish>>,
    _parents: Vec<DirEntry>,
}

impl<T> NamespaceDeferred<T> {
    pub fn finish(self) -> NamespaceOutcome<T> {
        let (outcome, completion) = self.finish_step();
        outcome.with_completion(completion)
    }

    /// Runs only phase four, retaining its new result independently of old errors.
    pub(crate) fn finish_step(self) -> (NamespaceOutcome<T>, VfsResult<()>) {
        let completion = self.finish.map_or(Ok(()), |finish| finish.finish());
        (self.outcome, completion)
    }
}
