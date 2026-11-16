//! Writable-open preparation uses the description's original write graph.

use crate::{
    BackingQueryContext, Location, Metadata, MutationCredentials, MutationRetire,
    NamespaceAdmission, NamespaceCommandScope, NamespaceCompletion, NamespaceObservation,
    NamespaceOperationContext, NamespaceOutcome, NamespacePreparationContext, NamespaceQuery,
    NamespaceRead, NamespaceReadResult, NamespaceState, NodeOps, NodePermission, PreparedWrite,
    VfsError, VfsResult,
};

/// A fresh observation and optional materialization under the real owner gate.
/// The owner must release that gate before the enclosing completion is finished.
pub trait LockedFileOpenAttempt {
    fn observe(&mut self) -> VfsResult<Metadata>;
    fn commit(&mut self, open: AuthorizedFileOpen<'_>) -> NamespaceOutcome<Metadata>;
}

/// One accepted open attempt. It cannot outlive or replace the bound request.
pub struct AuthorizedFileOpen<'a> {
    request: &'a FileOpenRequest<'a>,
}

/// Privately constructed request for an existing writable regular file.
/// Copy preparation may survive retries; authorization never does.
pub struct FileOpenRequest<'a> {
    target: Location,
    access: NodePermission,
    credentials: MutationCredentials<'a>,
    context: &'a NamespaceOperationContext<'a>,
}

impl<'a> FileOpenRequest<'a> {
    pub fn target(&self) -> &Location {
        &self.target
    }

    pub fn context(&self) -> &'a NamespaceOperationContext<'a> {
        self.context
    }

    pub fn admission(&self) -> &'a NamespaceAdmission<'a> {
        self.context
            .admission(&self.target)
            .expect("bound open receiver")
    }

    pub fn backing_queries(&self) -> BackingQueryContext<'a> {
        BackingQueryContext::new(self.context, self.admission())
    }

    pub fn preparations(&self) -> NamespacePreparationContext<'a> {
        self.context.preparations_for(self.admission().owner)
    }

    pub fn retired(&self) -> &'a MutationRetire {
        self.context.retired()
    }

    /// Observe and authorize anew while the backend retains its real gate.
    pub fn execute(&self, attempt: &mut dyn LockedFileOpenAttempt) -> NamespaceOutcome<Metadata> {
        let observed = match attempt.observe() {
            Ok(observed) => observed,
            Err(error) => return NamespaceOutcome::unchanged(Err(error)),
        };
        if let Err(error) = self.authorize(&observed) {
            return NamespaceOutcome::unchanged_observed(observed, Err(error));
        }
        attempt.commit(AuthorizedFileOpen { request: self })
    }

    fn authorize(&self, metadata: &Metadata) -> VfsResult<()> {
        if metadata.node_type != crate::NodeType::RegularFile {
            return Err(VfsError::InvalidInput);
        }
        self.credentials.check_permission(metadata, self.access)
    }

    /// Complete at the actual native receiver. The admitted query supplies its
    /// current metadata and any journal pressure to the original outer scope.
    pub fn native<N: NodeOps + ?Sized>(&self, owner: &N) -> NamespaceCompletion<Metadata> {
        let outcome = (|| {
            self.admission().check_owner(owner.filesystem())?;
            if owner.filesystem().stack_depth() != 0
                || owner.inode() != self.target.inode()
                || owner.identity().as_ref() != self.target.entry().identity()
            {
                return Err(VfsError::BadState);
            }
            Ok(
                match self
                    .context
                    .query(NamespaceQuery::Metadata(self.target.entry()))?
                {
                    NamespaceReadResult::Retry => NamespaceOutcome::retry(),
                    NamespaceReadResult::Ready(Err(error)) => {
                        NamespaceOutcome::unchanged(Err(error))
                    }
                    NamespaceReadResult::Ready(Ok(NamespaceRead::Metadata(metadata))) => {
                        let result = self.authorize(&metadata);
                        NamespaceOutcome::unchanged_observed(metadata, result)
                    }
                    NamespaceReadResult::Ready(Ok(_)) => {
                        NamespaceOutcome::unchanged(Err(VfsError::BadState))
                    }
                },
            )
        })();
        NamespaceCompletion::new(
            outcome.unwrap_or_else(|error| NamespaceOutcome::unchanged(Err(error))),
            None,
        )
    }
}

impl AuthorizedFileOpen<'_> {
    /// Prepare only this receiver's declared writable backing, using the saved
    /// mounter and the same exact-mount write graph. No destination gate is held.
    pub fn forward_to(
        &self,
        target: &Location,
        mounter: &MutationCredentials<'_>,
    ) -> VfsResult<NamespaceObservation<Metadata>> {
        let request = self.request;
        request.context.retired().retire(target.clone());
        request.context.write_for(target)?.check_location(target)?;
        let source = request.target.filesystem();
        let destination = target.filesystem();
        if core::ptr::addr_eq(source, destination)
            || source.stack_depth() > 2
            || destination.stack_depth() >= source.stack_depth()
            || !source.write_backing_roots().iter().any(|root| {
                core::ptr::addr_eq(root.filesystem(), destination)
                    && alloc::sync::Arc::ptr_eq(root.mountpoint(), target.mountpoint())
            })
        {
            return Err(VfsError::BadState);
        }
        request
            .context
            .forward_file_open(target, request.access, mounter)
    }
}

pub(crate) fn dispatch(
    context: &NamespaceOperationContext<'_>,
    target: &Location,
    access: NodePermission,
    credentials: &MutationCredentials<'_>,
) -> VfsResult<NamespaceCompletion<Metadata>> {
    let request = FileOpenRequest {
        target: target.clone(),
        access,
        credentials: *credentials,
        context,
    };
    Ok(target.entry().as_file()?.prepare_write_open(&request))
}

impl Location {
    /// Materialize an existing writable file before publishing its description.
    /// Borrow the very same preparation retained by that writable description;
    /// this operation never obtains another writer graph or publishes an fd.
    pub fn prepare_write_open(
        &self,
        write: &PreparedWrite,
        access: NodePermission,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        NamespaceCommandScope::run_write(write, |command| {
            loop {
                let scope = command.attempt::<Metadata>()?;
                {
                    let context = scope.context();
                    scope.prepare_file_open(&context, self, access, credentials)?;
                }
                let outcome = scope.finish().map_err(|_| VfsError::BadState)?;
                if outcome.state() != NamespaceState::Retry {
                    return Ok(outcome);
                }
                command.prepare()?;
            }
        })?
        .into_result()
    }
}
