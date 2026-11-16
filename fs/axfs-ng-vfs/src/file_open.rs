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

#[cfg(test)]
mod tests {
    use alloc::{
        sync::{Arc, Weak},
        vec,
    };
    use core::{
        any::Any,
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };
    use std::sync::{Mutex, mpsc};

    use axpoll::{IoEvents, Pollable};

    use super::*;
    use crate::{
        DirEntry, FileNode, FileNodeOps, Filesystem, FilesystemOps, MetadataUpdate, Mountpoint,
        NamespacePreparation, NodeIdentity, NodeType, Reference, StatFs,
    };

    // Models only owner exclusion and hidden preparation. The production scope
    // and authorizer decide whether a fresh attempt may publish an accepted open.
    struct Owner {
        this: Weak<Self>,
        metadata: Mutex<Metadata>,
        preparation: Arc<Preparation>,
        opened: AtomicBool,
    }

    struct Preparation {
        ready: AtomicBool,
        begin: mpsc::Sender<()>,
        done: Mutex<mpsc::Receiver<()>>,
    }

    impl NamespacePreparation for Preparation {
        fn prepare(&self, _: &PreparedWrite) -> VfsResult<()> {
            self.begin.send(()).unwrap();
            self.done
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            self.ready.store(true, Ordering::Relaxed);
            Ok(())
        }
        fn cleanup(&self, _: &PreparedWrite) -> VfsResult<()> {
            Ok(())
        }
        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl FilesystemOps for Owner {
        fn name(&self) -> &str {
            "open-preparation-owner"
        }
        fn root_dir(&self) -> DirEntry {
            DirEntry::new_file(
                FileNode::new(Arc::new(Node(self.this.upgrade().unwrap()))),
                NodeType::RegularFile,
                Reference::root(),
            )
        }
        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::Unsupported)
        }
    }

    struct Node(Arc<Owner>);

    impl NodeOps for Node {
        fn inode(&self) -> u64 {
            1
        }
        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(self.0.metadata.lock().unwrap().clone())
        }
        fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
            Err(VfsError::Unsupported)
        }
        fn filesystem(&self) -> &dyn FilesystemOps {
            self.0.as_ref()
        }
        fn sync(&self, _: bool) -> VfsResult<()> {
            Ok(())
        }
        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl Pollable for Node {
        fn poll(&self) -> IoEvents {
            IoEvents::IN | IoEvents::OUT
        }
        unsafe fn register_shared(&self, _: &mut dyn axpoll::SharedRegistrationSink, _: IoEvents) {}
    }

    impl FileNodeOps for Node {
        fn prepare_write_open(
            &self,
            request: &FileOpenRequest<'_>,
        ) -> NamespaceCompletion<Metadata> {
            request.admission().check_owner(self.0.as_ref()).unwrap();
            let metadata = self.0.metadata.lock().unwrap();
            NamespaceCompletion::new(
                request.execute(&mut Attempt {
                    owner: &self.0,
                    metadata: &metadata,
                    request,
                }),
                None,
            )
        }
        fn read_at(&self, _: &mut [u8], _: u64) -> VfsResult<usize> {
            Err(VfsError::Unsupported)
        }
        fn write_at(&self, _: &[u8], _: u64) -> VfsResult<usize> {
            Err(VfsError::Unsupported)
        }
        fn append(&self, _: &[u8]) -> VfsResult<(usize, u64)> {
            Err(VfsError::Unsupported)
        }
        fn set_len(&self, _: u64) -> VfsResult<()> {
            Err(VfsError::Unsupported)
        }
    }

    struct Attempt<'owner, 'request, 'op> {
        owner: &'owner Owner,
        metadata: &'owner Metadata,
        request: &'request FileOpenRequest<'op>,
    }

    impl LockedFileOpenAttempt for Attempt<'_, '_, '_> {
        fn observe(&mut self) -> VfsResult<Metadata> {
            Ok(self.metadata.clone())
        }
        fn commit(&mut self, _: AuthorizedFileOpen<'_>) -> NamespaceOutcome<Metadata> {
            if !self.owner.preparation.ready.load(Ordering::Relaxed) {
                let result = self
                    .request
                    .preparations()
                    .insert(NodeIdentity::new(vec![1]), self.owner.preparation.clone());
                return match result {
                    Ok(_) => NamespaceOutcome::retry(),
                    Err(error) => NamespaceOutcome::unchanged(Err(error)),
                };
            }
            self.owner.opened.store(true, Ordering::Relaxed);
            NamespaceOutcome::unchanged_observed(self.metadata.clone(), Ok(()))
        }
    }

    #[test]
    fn preparation_retries_reauthorize_before_open_publication() {
        let (begin, changing) = mpsc::channel();
        let (changed, done) = mpsc::channel();
        let preparation = Arc::new(Preparation {
            ready: AtomicBool::new(false),
            begin,
            done: Mutex::new(done),
        });
        let owner = Arc::new_cyclic(|this| Owner {
            this: this.clone(),
            preparation,
            opened: AtomicBool::new(false),
            metadata: Mutex::new(Metadata {
                device: 1,
                inode: 1,
                nlink: 1,
                mode: NodePermission::from_bits_truncate(0o600),
                node_type: NodeType::RegularFile,
                uid: 1000,
                gid: 1000,
                size: 0,
                block_size: 4096,
                blocks: 0,
                rdev: crate::DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            }),
        });
        let location = Mountpoint::new_root(&Filesystem::new(owner.clone())).root_location();
        let write = PreparedWrite::acquire(&location).unwrap();
        let credentials = MutationCredentials {
            fsuid: 1000,
            fsgid: 1000,
            cap_dac_override: false,
            cap_dac_read_search: false,
            ..MutationCredentials::root()
        };
        let actor = {
            let owner = owner.clone();
            std::thread::spawn(move || {
                changing.recv_timeout(Duration::from_secs(10)).unwrap();
                // This separate actor changes visible metadata only after the
                // first owner gate and admission have ended for preparation.
                owner.metadata.lock().unwrap().mode = NodePermission::from_bits_truncate(0o400);
                changed.send(()).unwrap();
            })
        };
        let result = location.prepare_write_open(&write, NodePermission::OTHER_WRITE, &credentials);
        actor.join().unwrap();
        assert_eq!(result, Err(VfsError::PermissionDenied));
        assert!(
            !owner.opened.load(Ordering::Relaxed),
            "revoked permission must prevent publication"
        );
        owner.metadata.lock().unwrap().mode = NodePermission::from_bits_truncate(0o600);
        location
            .prepare_write_open(&write, NodePermission::OTHER_WRITE, &credentials)
            .unwrap();
        assert!(owner.opened.load(Ordering::Relaxed));
        drop(write);
        location.mountpoint().set_readonly(true).unwrap();
    }
}
