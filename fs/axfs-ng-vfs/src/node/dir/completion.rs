//! Typed backend work survives the end of namespace exclusion.

use alloc::{boxed::Box, vec::Vec};

use super::{NamespaceEffect, NamespaceOutcome};
use crate::{
    DirEntry, FilesystemOps, NamespaceAdmissions, VfsResult, namespace_admission::owner_address,
};

/// Backend-owned typed pressure/abort state. This object holds no backend gate,
/// slot, authorization or name publication scope. It must not reenter admission.
pub trait NamespacePressure<T = NamespaceEffect> {
    /// Consumes phase two and returns only admission-independent owners.
    fn pressure(
        self: Box<Self>,
        outcome: &mut NamespaceOutcome<T>,
    ) -> Option<Box<dyn NamespaceFinish>>;
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

    pub(super) fn bind(mut self, owner: &dyn FilesystemOps) -> Self {
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

    pub(crate) fn pressure(mut self) -> NamespaceDeferred<T> {
        let finish = self
            .pressure
            .take()
            .and_then(|pressure| pressure.pressure(&mut self.outcome));
        NamespaceDeferred {
            outcome: self.outcome,
            finish,
            _parents: self.parents,
        }
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
        let completion = self.finish.map_or(Ok(()), |finish| finish.finish());
        self.outcome.with_completion(completion)
    }
}
