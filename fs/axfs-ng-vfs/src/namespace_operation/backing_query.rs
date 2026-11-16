//! Receiver-bound reads through the operation's already admitted backing graph.

use super::{NamespaceOperationContext, NamespaceReadResult, registration::Reservation};
use crate::{
    Location, MutationRetire, NamespaceAdmission, NamespaceCompletion, NamespaceOutcome,
    NamespaceQuery, NamespaceRead, NamespaceState, VfsError, VfsResult,
};

/// Borrow-only queries to the current receiver's direct backing filesystems.
///
/// Only strict ancestor gates may remain held: the destination must have no
/// live driver/gate or unconsumed child observation. This view neither proves
/// that guard discipline nor authorizes mutations from the returned snapshots.
/// It borrows the original operation, not the request that produced it, and
/// cannot extend the request's authorization or the scope's lifetime.
pub struct BackingQueryContext<'op> {
    context: &'op NamespaceOperationContext<'op>,
    source: &'op NamespaceAdmission<'op>,
    depth: u32,
}

impl<'op> BackingQueryContext<'op> {
    pub(crate) fn new(
        context: &'op NamespaceOperationContext<'op>,
        source: &'op NamespaceAdmission<'op>,
    ) -> Self {
        Self {
            context,
            source,
            depth: source.owner.stack_depth(),
        }
    }

    /// Retains logical candidates until every enclosing gate and admission ends.
    /// This view cannot clear the outer operation's retirement collection.
    pub fn retired(&self) -> &'op MutationRetire {
        self.context.retired()
    }

    /// Reads an admitted direct dependency with strictly smaller stack depth.
    ///
    /// Missing admission precedes relationship errors, which precede Lookup
    /// name errors. Read-only backing roles need no write lease. Root/path
    /// provenance remains the layered backend's private adapter responsibility.
    pub fn query(
        &self,
        query: NamespaceQuery<'_>,
    ) -> VfsResult<NamespaceReadResult<NamespaceRead>> {
        self.context.retired().retire(query.entry().clone());
        let destination = self
            .context
            .admission_for_owner(query.entry().filesystem())?;
        let source = self.source.owner;
        let depth = destination.owner.stack_depth();
        if self.source.check_owner(destination.owner).is_ok()
            || self.depth > 2
            || depth > 2
            || depth >= self.depth
            || !source
                .read_backing_roots()
                .iter()
                .chain(source.write_backing_roots())
                .any(|root| destination.check_owner(root.filesystem()).is_ok())
        {
            return Err(VfsError::BadState);
        }
        BackingQueryRequest::new(self.context, destination, query).dispatch()
    }

    /// Looks up one component in the parent's exact mount, without following
    /// covered mounts or final links. All inputs/candidates share outer retire.
    pub fn lookup_backing(
        &self,
        parent: &Location,
        name: &str,
    ) -> VfsResult<NamespaceReadResult<Location>> {
        self.context.retired().retire(parent.clone());
        Ok(self
            .query(NamespaceQuery::Lookup {
                parent: parent.entry(),
                name,
            })?
            .and_then(|read| fixed_lookup(parent, read)))
    }
}

/// A privately bound read request for one actual destination receiver.
///
/// Composite hooks must check `admission().check_owner(self)` before taking
/// their gate. Further reads use this request's receiver-bound view, never the
/// ancestor's source. This request carries no completion or write authority.
pub struct BackingQueryRequest<'op, 'query> {
    context: &'op NamespaceOperationContext<'op>,
    admission: &'op NamespaceAdmission<'op>,
    query: NamespaceQuery<'query>,
}

impl<'op, 'query> BackingQueryRequest<'op, 'query> {
    pub(super) fn new(
        context: &'op NamespaceOperationContext<'op>,
        admission: &'op NamespaceAdmission<'op>,
        query: NamespaceQuery<'query>,
    ) -> Self {
        Self {
            context,
            admission,
            query,
        }
    }

    pub fn query(&self) -> &NamespaceQuery<'query> {
        &self.query
    }

    pub fn admission(&self) -> &'op NamespaceAdmission<'op> {
        self.admission
    }

    pub fn retired(&self) -> &'op MutationRetire {
        self.context.retired()
    }

    /// Rebinds further reads to this destination, independently of the query's
    /// shorter entry borrow and without borrowing this request's address.
    pub fn backing_queries(&self) -> BackingQueryContext<'op> {
        BackingQueryContext::new(self.context, self.admission)
    }

    /// Consumes the request at its fixed native owner, without reentering
    /// admission or calling the composite hook again. Temporary backend owners,
    /// including error-path owners, must go into the same retirement bag.
    pub fn execute_native(self) -> NamespaceCompletion<NamespaceRead> {
        let result = self.admission.owner.query_namespace(
            self.admission,
            self.query,
            self.context.retired(),
        );
        NamespaceCompletion::new(
            match result {
                Ok(read) => NamespaceOutcome::unchanged_observed(read, Ok(())),
                Err(error) => NamespaceOutcome::unchanged(Err(error)),
            },
            None,
        )
    }

    pub(super) fn dispatch(self) -> VfsResult<NamespaceReadResult<NamespaceRead>> {
        if let NamespaceQuery::Lookup { name, .. } = &self.query {
            crate::path::verify_entry_name(name)?;
        }
        let kind = match &self.query {
            NamespaceQuery::Metadata(_) => ReadKind::Metadata,
            NamespaceQuery::ReadLink(_) => ReadKind::ReadLink,
            NamespaceQuery::Lookup { .. } => ReadKind::Lookup,
            NamespaceQuery::Xattr { .. } => ReadKind::Xattr,
            NamespaceQuery::Directory { cursor, .. } => ReadKind::Directory(*cursor),
        };
        let admission = self.admission;
        let retired = self.context.retired();
        let mut reservation = Reservation::child(self.context.children)?;
        reservation.start();
        let completion = admission
            .owner
            .query_backing_namespace(self)
            .bind(admission.owner);
        let outcome = completion.outcome();
        // Even an Entry returned for Metadata/Xattr must survive all gates.
        if let Some(NamespaceRead::Entry(Some(entry))) = outcome.observation() {
            retired.retire(entry.clone());
        }
        let validation = match (outcome.state(), outcome.observation()) {
            (NamespaceState::Retry, None) => Ok(()),
            (NamespaceState::Unchanged, None) if outcome.result().is_err() => Ok(()),
            (NamespaceState::Unchanged, Some(read)) => match (kind, read) {
                (ReadKind::Metadata, NamespaceRead::Metadata(_))
                | (ReadKind::ReadLink, NamespaceRead::LinkText(_))
                | (ReadKind::Xattr, NamespaceRead::Xattr(_))
                | (ReadKind::Lookup, NamespaceRead::Entry(None)) => Ok(()),
                (ReadKind::Lookup, NamespaceRead::Entry(Some(entry))) => {
                    admission.check_owner(entry.filesystem())
                }
                (ReadKind::Directory(cursor), NamespaceRead::Directory(batch)) => {
                    batch.validate(cursor)
                }
                _ => Err(VfsError::BadState),
            },
            _ => Err(VfsError::BadState),
        };
        Ok(reservation.read(completion.with_validation(validation)))
    }
}

enum ReadKind {
    Metadata,
    ReadLink,
    Lookup,
    Xattr,
    Directory(crate::DirectoryCursor),
}

pub(super) fn fixed_lookup(parent: &Location, result: NamespaceRead) -> VfsResult<Location> {
    // Dispatch already retained and checked every candidate and response shape.
    match result {
        NamespaceRead::Entry(Some(entry)) => Ok(Location::new(parent.mountpoint().clone(), entry)),
        NamespaceRead::Entry(None) => Err(VfsError::NotFound),
        _ => Err(VfsError::BadState),
    }
}
