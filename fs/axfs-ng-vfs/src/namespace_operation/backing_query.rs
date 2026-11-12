//! Receiver-bound reads through the operation's already admitted backing graph.

use super::NamespaceOperationContext;
use crate::{
    Location, MutationRetire, NamespaceAdmission, NamespaceQuery, NamespaceRead, VfsError,
    VfsResult,
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

    /// Reads an admitted direct dependency with strictly smaller stack depth.
    ///
    /// Missing admission precedes relationship errors, which precede Lookup
    /// name errors. Read-only backing roles need no write lease. Root/path
    /// provenance remains the layered backend's private adapter responsibility.
    pub fn query(&self, query: NamespaceQuery<'_>) -> VfsResult<NamespaceRead> {
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
    pub fn lookup_backing(&self, parent: &Location, name: &str) -> VfsResult<Location> {
        self.context.retired().retire(parent.clone());
        fixed_lookup(
            parent,
            self.query(NamespaceQuery::Lookup {
                parent: parent.entry(),
                name,
            })?,
        )
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
    pub fn execute_native(self) -> VfsResult<NamespaceRead> {
        self.admission
            .owner
            .query_namespace(self.admission, self.query, self.context.retired())
    }

    pub(super) fn dispatch(self) -> VfsResult<NamespaceRead> {
        if let NamespaceQuery::Lookup { name, .. } = &self.query {
            crate::path::verify_entry_name(name)?;
        }
        let kind = match &self.query {
            NamespaceQuery::Metadata(_) => ReadKind::Metadata,
            NamespaceQuery::Lookup { .. } => ReadKind::Lookup,
            NamespaceQuery::Xattr { .. } => ReadKind::Xattr,
        };
        let admission = self.admission;
        let retired = self.context.retired();
        let result = admission.owner.query_backing_namespace(self)?;
        // Even an Entry returned for Metadata/Xattr must survive all gates.
        if let NamespaceRead::Entry(Some(entry)) = &result {
            retired.retire(entry.clone());
        }
        match (kind, &result) {
            (ReadKind::Metadata, NamespaceRead::Metadata(_))
            | (ReadKind::Xattr, NamespaceRead::Xattr(_))
            | (ReadKind::Lookup, NamespaceRead::Entry(None)) => {}
            (ReadKind::Lookup, NamespaceRead::Entry(Some(entry))) => {
                admission.check_owner(entry.filesystem())?;
            }
            _ => return Err(VfsError::BadState),
        }
        Ok(result)
    }
}

enum ReadKind {
    Metadata,
    Lookup,
    Xattr,
}

pub(super) fn fixed_lookup(parent: &Location, result: NamespaceRead) -> VfsResult<Location> {
    // Dispatch already retained and checked every candidate and response shape.
    match result {
        NamespaceRead::Entry(Some(entry)) => Ok(Location::new(parent.mountpoint().clone(), entry)),
        NamespaceRead::Entry(None) => Err(VfsError::NotFound),
        _ => Err(VfsError::BadState),
    }
}
