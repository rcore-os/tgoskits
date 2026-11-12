//! Non-copyable reservations and allocation-free completion phase transitions.

use alloc::vec::Vec;
use core::cell::RefCell;

use super::NamespaceObservation;
use crate::{
    Metadata, NamespaceAdmissions, NamespaceCompletion, NamespaceDeferred, NamespaceEffect,
    VfsError, VfsResult,
};

pub(super) struct Registration {
    pub(super) children: Vec<TypedWork>,
    pub(super) reservations: usize,
    pub(super) incomplete: bool,
    pub(super) root_claimed: bool,
}

impl Registration {
    pub(super) fn new() -> Self {
        Self {
            children: Vec::new(),
            reservations: 0,
            incomplete: false,
            root_claimed: false,
        }
    }

    pub(super) fn admitted_by(&self, admissions: &NamespaceAdmissions<'_>) -> bool {
        self.children.iter().all(|child| match child {
            TypedWork::Namespace(Some(child)) => child.admitted_by(admissions),
            TypedWork::Metadata(Some(child)) => child.admitted_by(admissions),
            _ => unreachable!("preflight before phase transitions"),
        })
    }
}

pub(super) enum TypedWork {
    Namespace(Option<NamespaceCompletion>),
    Metadata(Option<NamespaceCompletion<Metadata>>),
    NamespaceDeferred(Option<NamespaceDeferred>),
    MetadataDeferred(Option<NamespaceDeferred<Metadata>>),
    Finished,
}

impl TypedWork {
    pub(super) fn pressure(&mut self) -> VfsResult<()> {
        let (next, result) = match self {
            Self::Namespace(completion) => {
                let (deferred, result) = completion.take().expect("pending child").pressure_step();
                (Self::NamespaceDeferred(Some(deferred)), result)
            }
            Self::Metadata(completion) => {
                let (deferred, result) = completion.take().expect("pending child").pressure_step();
                (Self::MetadataDeferred(Some(deferred)), result)
            }
            _ => unreachable!("pressure once"),
        };
        *self = next;
        result
    }

    pub(super) fn finish(&mut self) -> VfsResult<()> {
        let result = match self {
            Self::NamespaceDeferred(deferred) => {
                deferred.take().expect("pressured child").finish_step().1
            }
            Self::MetadataDeferred(deferred) => {
                deferred.take().expect("pressured child").finish_step().1
            }
            _ => unreachable!("finish after pressure"),
        };
        *self = Self::Finished;
        result
    }
}

enum Destination<'a> {
    Child,
    Namespace(&'a RefCell<Option<NamespaceCompletion>>),
    Metadata(&'a RefCell<Option<NamespaceCompletion<Metadata>>>),
}

/// Created before backend entry. A started but unreturned receipt leaves a
/// sticky mark, even after its reservation is released and a root is present.
pub(crate) struct Reservation<'a> {
    registry: &'a RefCell<Registration>,
    destination: Destination<'a>,
    started: bool,
    returned: bool,
}

impl<'a> Reservation<'a> {
    pub(super) fn child(registry: &'a RefCell<Registration>) -> VfsResult<Self> {
        let mut registration = registry.borrow_mut();
        let pending = registration
            .reservations
            .checked_add(1)
            .ok_or(VfsError::NoMemory)?;
        // Vec::try_reserve is relative to len, not capacity. Every outstanding
        // reservation remains accounted for while a recursive child returns.
        registration
            .children
            .try_reserve(pending)
            .map_err(|_| VfsError::NoMemory)?;
        registration.reservations = pending;
        Ok(Self::new(registry, Destination::Child))
    }

    pub(super) fn root_namespace(
        registry: &'a RefCell<Registration>,
        root: &'a RefCell<Option<NamespaceCompletion>>,
    ) -> Self {
        registry.borrow_mut().root_claimed = true;
        Self::new(registry, Destination::Namespace(root))
    }

    pub(super) fn root_metadata(
        registry: &'a RefCell<Registration>,
        root: &'a RefCell<Option<NamespaceCompletion<Metadata>>>,
    ) -> Self {
        registry.borrow_mut().root_claimed = true;
        Self::new(registry, Destination::Metadata(root))
    }

    fn new(registry: &'a RefCell<Registration>, destination: Destination<'a>) -> Self {
        Self {
            registry,
            destination,
            started: false,
            returned: false,
        }
    }

    pub(crate) fn start(&mut self) {
        self.started = true;
    }

    pub(crate) fn namespace(
        mut self,
        completion: NamespaceCompletion,
    ) -> NamespaceObservation<NamespaceEffect> {
        match &self.destination {
            Destination::Child => {
                let mut registry = self.registry.borrow_mut();
                registry
                    .children
                    .push(TypedWork::Namespace(Some(completion)));
                self.returned = true;
                match registry.children.last().expect("registered child") {
                    TypedWork::Namespace(Some(completion)) => {
                        NamespaceObservation::capture(completion.outcome())
                    }
                    _ => unreachable!("namespace child"),
                }
            }
            Destination::Namespace(root) => {
                assert!(root.borrow().is_none(), "one root per attempt");
                *root.borrow_mut() = Some(completion);
                self.returned = true;
                NamespaceObservation::capture(
                    root.borrow().as_ref().expect("registered root").outcome(),
                )
            }
            Destination::Metadata(_) => unreachable!("typed namespace destination"),
        }
    }

    pub(crate) fn metadata(
        mut self,
        completion: NamespaceCompletion<Metadata>,
    ) -> NamespaceObservation<Metadata> {
        match &self.destination {
            Destination::Child => {
                let mut registry = self.registry.borrow_mut();
                registry
                    .children
                    .push(TypedWork::Metadata(Some(completion)));
                self.returned = true;
                match registry.children.last().expect("registered child") {
                    TypedWork::Metadata(Some(completion)) => {
                        NamespaceObservation::capture(completion.outcome())
                    }
                    _ => unreachable!("metadata child"),
                }
            }
            Destination::Metadata(root) => {
                assert!(root.borrow().is_none(), "one root per attempt");
                *root.borrow_mut() = Some(completion);
                self.returned = true;
                NamespaceObservation::capture(
                    root.borrow().as_ref().expect("registered root").outcome(),
                )
            }
            Destination::Namespace(_) => unreachable!("typed metadata destination"),
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut registry = self.registry.borrow_mut();
        if matches!(self.destination, Destination::Child) {
            registry.reservations -= 1;
        } else if !self.started {
            registry.root_claimed = false;
        }
        registry.incomplete |= self.started && !self.returned;
    }
}
