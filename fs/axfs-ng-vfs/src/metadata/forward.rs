//! Borrowed forwarding preserves setattr intentions across real owner layers.

use super::*;
use crate::{NamespaceObservation, VfsError};

#[derive(Clone, Copy, Default)]
pub(crate) struct SetidRemoval {
    pub suid: bool,
    pub sgid: bool,
}

/// Only the fixed authorizer can produce an intention for the next layer.
#[derive(Clone, Copy)]
pub(crate) enum ForwardedChange {
    Chmod(NodePermission),
    Chown {
        uid: Option<u32>,
        gid: Option<u32>,
        removal: SetidRemoval,
    },
    Times {
        atime: TimeChange,
        mtime: TimeChange,
    },
}

impl ForwardedChange {
    pub(super) const fn changes_permissions_or_ownership(self) -> bool {
        !matches!(self, Self::Times { .. })
    }
}

impl AuthorizedMetadataChange<'_> {
    /// Forwards this attempt's accepted intention to a declared writable backing.
    ///
    /// The child observes and authorizes under its own real exclusion using the
    /// saved mounter. Filtered chmod bits and original chown kill flags survive
    /// the hop; NOW remains an intention until that owner commits. This call
    /// borrows the current authorization and registers the child's completion
    /// in the same outer scope. No destination driver/gate may remain held.
    pub fn forward_to(
        &self,
        target: &Location,
        mounter: &MutationCredentials<'_>,
    ) -> VfsResult<NamespaceObservation<Metadata>> {
        let request = self._request;
        let context = request.context;
        context.retired().retire(target.clone());
        // Check the original exact-mount lease before the direct dependency.
        // The composite adapter must select its bound upper entity, including
        // unlinked entities; mutable name ancestry is not that provenance.
        context.write_for(target)?.check_location(target)?;
        let source = request.target.filesystem();
        let destination = target.filesystem();
        let source_address = source as *const dyn crate::FilesystemOps as *const ();
        let destination_address = destination as *const dyn crate::FilesystemOps as *const ();
        if source_address == destination_address
            || source.stack_depth() > 2
            || destination.stack_depth() >= source.stack_depth()
            || !source.write_backing_roots().iter().any(|root| {
                core::ptr::addr_eq(root.filesystem(), destination)
                    && alloc::sync::Arc::ptr_eq(root.mountpoint(), target.mountpoint())
            })
        {
            return Err(VfsError::BadState);
        }
        let intent = match &request.intent {
            MetadataIntent::Initialization(update) => {
                MetadataIntent::Initialization(update.clone())
            }
            MetadataIntent::StagingRestoration(update) => {
                let mut update = update.clone();
                update.mode = self.mode;
                MetadataIntent::StagingRestoration(update)
            }
            MetadataIntent::User(MetadataChange::Chmod(_))
            | MetadataIntent::Forwarded(ForwardedChange::Chmod(_)) => MetadataIntent::Forwarded(
                ForwardedChange::Chmod(self.mode.expect("authorized explicit chmod")),
            ),
            MetadataIntent::User(MetadataChange::Chown { uid, gid })
            | MetadataIntent::Forwarded(ForwardedChange::Chown { uid, gid, .. }) => {
                MetadataIntent::Forwarded(ForwardedChange::Chown {
                    uid: *uid,
                    gid: *gid,
                    removal: self.removal,
                })
            }
            MetadataIntent::User(MetadataChange::Times { atime, mtime })
            | MetadataIntent::Forwarded(ForwardedChange::Times { atime, mtime }) => {
                MetadataIntent::Forwarded(ForwardedChange::Times {
                    atime: *atime,
                    mtime: *mtime,
                })
            }
        };
        context.forward_metadata(target, intent, mounter)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{
        sync::{Arc, Weak},
        vec,
        vec::Vec,
    };
    use core::{
        any::Any,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use std::sync::Mutex;

    use axpoll::{IoEvents, Pollable};

    use super::*;
    use crate::{
        DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, FileNodeOps,
        Filesystem, FilesystemOps, Mountpoint, NodeOps, NodeType, Reference, StatFs,
    };

    // This component adapter exercises the real scope, authorizer and child
    // dispatch. It models locked metadata only, never Overlay copy-up or I/O.
    struct Owner {
        this: Weak<Self>,
        backing: Option<Location>,
        writes: Vec<Location>,
        reads: Vec<Location>,
        mounter: MutationCredentials<'static>,
        state: Mutex<Metadata>,
        gate: Mutex<()>,
        clock: u64,
        commits: AtomicUsize,
    }

    impl FilesystemOps for Owner {
        fn name(&self) -> &str {
            "metadata-forwarding-probe"
        }
        fn root_dir(&self) -> DirEntry {
            let node = Arc::new(Node(self.this.upgrade().unwrap()));
            DirEntry::new_dir(|_| DirNode::new(node), Reference::root())
        }
        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::Unsupported)
        }
        fn stack_depth(&self) -> u32 {
            self.backing
                .as_ref()
                .map_or(0, |loc| loc.filesystem().stack_depth() + 1)
        }
        fn write_backing_roots(&self) -> &[Location] {
            &self.writes
        }
        fn read_backing_roots(&self) -> &[Location] {
            &self.reads
        }
    }

    struct Node(Arc<Owner>);
    impl NodeOps for Node {
        fn inode(&self) -> u64 {
            1
        }
        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(self.0.state.lock().unwrap().clone())
        }
        fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
            Err(VfsError::Unsupported)
        }
        fn sync(&self, _: bool) -> VfsResult<()> {
            Ok(())
        }
        fn filesystem(&self) -> &dyn FilesystemOps {
            self.0.as_ref()
        }
        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
        fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
            request.admission().check_owner(self.0.as_ref()).unwrap();
            let _guard = self.0.gate.lock().unwrap();
            NamespaceCompletion::new(request.execute(&mut Attempt(self)), None)
        }
    }
    impl DirNodeOps for Node {
        fn lookup(&self, _: &str) -> VfsResult<DirEntry> {
            Err(VfsError::NotFound)
        }
        fn read_dir(&self, _: DirectoryCursor, _: &mut dyn DirEntrySink) -> VfsResult<usize> {
            Ok(0)
        }
    }
    impl Pollable for Node {
        fn poll(&self) -> IoEvents {
            IoEvents::empty()
        }
        unsafe fn register_shared(&self, _: &mut dyn axpoll::SharedRegistrationSink, _: IoEvents) {}
        unsafe fn register_exclusive(
            &self,
            _: &mut dyn axpoll::ExclusiveRegistrationSink,
            _: IoEvents,
        ) {
        }
    }
    impl FileNodeOps for Node {
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

    struct Attempt<'a>(&'a Node);
    impl LockedMetadataAttempt for Attempt<'_> {
        fn observe(&mut self) -> VfsResult<Metadata> {
            self.0.metadata()
        }
        fn wall_time(&mut self) -> Duration {
            Duration::from_secs(self.0.0.clock)
        }
        fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
            let owner = &self.0.0;
            if let Some(backing) = &owner.backing {
                let observed = match change.forward_to(backing, &owner.mounter) {
                    Ok(observed) => observed,
                    Err(error) => return NamespaceOutcome::unchanged(Err(error)),
                };
                let snapshot = observed.observation().cloned();
                return match observed.state() {
                    NamespaceState::Applied => {
                        NamespaceOutcome::applied(snapshot.unwrap(), observed.result())
                    }
                    NamespaceState::Unchanged => NamespaceOutcome::unchanged(observed.result()),
                    NamespaceState::Retry => NamespaceOutcome::retry(),
                    NamespaceState::Uncertain => {
                        NamespaceOutcome::uncertain(snapshot, observed.result())
                    }
                };
            }
            let mut state = owner.state.lock().unwrap();
            change.apply(&mut state);
            owner.commits.fetch_add(1, Ordering::Relaxed);
            NamespaceOutcome::applied(state.clone(), Ok(()))
        }
    }

    fn credentials(uid: u32, gid: u32) -> MutationCredentials<'static> {
        MutationCredentials {
            fsuid: uid,
            fsgid: gid,
            supplementary_gids: &[],
            cap_chown: false,
            cap_fowner: false,
            cap_fsetid: false,
            cap_dac_override: false,
            cap_dac_read_search: false,
            cap_mknod: false,
        }
    }

    fn owner(
        backing: Option<Location>,
        writable: bool,
        mode: u16,
        mounter: MutationCredentials<'static>,
        clock: u64,
    ) -> (Arc<Owner>, Location) {
        let fs = Arc::new_cyclic(|this| Owner {
            this: this.clone(),
            writes: if writable {
                backing.iter().cloned().collect()
            } else {
                vec![]
            },
            reads: if writable {
                vec![]
            } else {
                backing.iter().cloned().collect()
            },
            backing,
            mounter,
            state: Mutex::new(Metadata {
                device: 0,
                inode: 1,
                nlink: 1,
                mode: NodePermission::from_bits_truncate(mode),
                node_type: NodeType::RegularFile,
                uid: 5,
                gid: 88,
                size: 0,
                block_size: 4096,
                blocks: 0,
                rdev: DeviceId::default(),
                atime: Duration::from_secs(10),
                mtime: Duration::from_secs(20),
                ctime: Duration::from_secs(30),
            }),
            gate: Mutex::new(()),
            clock,
            commits: AtomicUsize::new(0),
        });
        let mount = Mountpoint::new_root(&Filesystem::new(fs.clone()));
        let node = DirEntry::new_file(
            FileNode::new(Arc::new(Node(fs.clone()))),
            NodeType::RegularFile,
            Reference::new(Some(mount.root_location().entry().clone()), "file".into()),
        );
        (fs, Location::new(mount, node))
    }

    #[test]
    fn forwarding_preserves_filtered_mode_kill_intent_and_final_owner_time() {
        for (mode, change, caller, mounter, expected) in [
            (
                0o644,
                MetadataChange::Chmod(NodePermission::from_bits_truncate(0o2644)),
                credentials(5, 6),
                MutationCredentials::root(),
                0o644,
            ),
            // The outer caller keeps non-executable SGID. Replaying chown as
            // the differently grouped mounter would incorrectly kill it.
            (
                0o2644,
                MetadataChange::Chown {
                    uid: None,
                    gid: None,
                },
                credentials(5, 88),
                MutationCredentials {
                    cap_fowner: true,
                    ..credentials(5, 6)
                },
                0o2644,
            ),
            // A privileged mounter must still perform the caller's kill intent.
            (
                0o2644,
                MetadataChange::Chown {
                    uid: None,
                    gid: None,
                },
                credentials(5, 6),
                MutationCredentials::root(),
                0o644,
            ),
        ] {
            let (native, native_path) = owner(None, true, mode, MutationCredentials::root(), 300);
            let (_, middle_path) = owner(Some(native_path.clone()), true, mode, mounter, 200);
            let (_, outer_path) = owner(
                Some(middle_path),
                true,
                mode,
                MutationCredentials::root(),
                100,
            );
            outer_path.change_metadata(change, &caller).unwrap();
            assert_eq!(native.state.lock().unwrap().mode.bits(), expected);
            assert_eq!(native.commits.load(Ordering::Relaxed), 1);
        }
        let (native, native_path) = owner(None, true, 0o644, MutationCredentials::root(), 300);
        let (_, outer_path) = owner(
            Some(native_path.clone()),
            true,
            0o644,
            MutationCredentials::root(),
            100,
        );
        outer_path
            .change_metadata(
                MetadataChange::Times {
                    atime: TimeChange::Now,
                    mtime: TimeChange::Keep,
                },
                &credentials(5, 6),
            )
            .unwrap();
        let state = native.state.lock().unwrap().clone();
        assert_eq!(
            (
                state.atime.as_secs(),
                state.mtime.as_secs(),
                state.ctime.as_secs()
            ),
            (300, 20, 300)
        );

        for (writable, mounter, error) in [
            (true, credentials(66, 6), VfsError::OperationNotPermitted),
            (false, MutationCredentials::root(), VfsError::CrossesDevices),
        ] {
            let (native, backing) = owner(None, true, 0o644, MutationCredentials::root(), 300);
            let (_, outer) = owner(Some(backing), writable, 0o644, mounter, 100);
            assert_eq!(
                outer.change_metadata(
                    MetadataChange::Chmod(NodePermission::from_bits_truncate(0o600)),
                    &MutationCredentials::root()
                ),
                Err(error)
            );
            assert_eq!(native.state.lock().unwrap().mode.bits(), 0o644);
            assert_eq!(native.commits.load(Ordering::Relaxed), 0);
        }
    }
}
