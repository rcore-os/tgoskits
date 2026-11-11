//! Metadata attempts share namespace exclusion and the real inode metadata.

use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, MetadataRequest, NamespaceCompletion,
    NamespaceOutcome,
};

use super::*;

struct MemoryMetadataAttempt<'a> {
    node: &'a MemoryNode,
    metadata: &'a mut Metadata,
    directory_size: Option<u64>,
}

impl MemoryNode {
    pub(super) fn run_metadata(
        &self,
        request: &MetadataRequest<'_>,
    ) -> NamespaceCompletion<Metadata> {
        #[cfg(all(test, feature = "host-test"))]
        super::tests::before_namespace_change();
        let outcome = {
            // Same order as namespace authorization. No inode owner is dropped
            // under either guard; the fixed request retains the whole Location.
            let _mutation = self.fs.mutation.lock();
            // Ordinary lookup takes entries -> inodes, and wrapper destruction
            // takes inodes -> metadata without the mutation gate. Never take
            // entries while holding metadata: that would close a three-way cycle.
            // The gate keeps this attempt-local directory size stable through
            // observation and commit; no owner fields are sampled outside metadata.
            let directory_size = self.directory_size();
            let mut metadata = self.inode.metadata.lock_irqsave();
            let mut attempt = MemoryMetadataAttempt {
                node: self,
                metadata: &mut metadata,
                directory_size,
            };
            request.execute(&mut attempt)
        };
        // Metadata is not a namespace publication and does not imply DIRSYNC.
        NamespaceCompletion::new(outcome, None)
    }

    // Called under the mutation gate, before taking the inode metadata lock.
    fn directory_size(&self) -> Option<u64> {
        let NodeContent::Dir(directory) = &self.inode.content else {
            return None;
        };
        #[cfg(all(test, feature = "host-test"))]
        tests::before_directory_size_read(self);
        let size = {
            let entries = directory.entries.lock_irqsave();
            entries.len() as u64
        };
        Some(size)
    }
}

impl MemoryMetadataAttempt<'_> {
    fn snapshot(&self) -> Metadata {
        let mut metadata = self.metadata.clone();
        metadata.size = match &self.node.inode.content {
            NodeContent::File(file) => file.length.load(AtomicOrdering::Acquire),
            NodeContent::Dir(_) => self
                .directory_size
                .expect("directory size sampled under the same mutation gate"),
        };
        metadata
    }
}

impl LockedMetadataAttempt for MemoryMetadataAttempt<'_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        Ok(self.snapshot())
    }

    fn wall_time(&mut self) -> Duration {
        crate::os::wall_time()
    }

    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
        change.apply(self.metadata);
        NamespaceOutcome::applied(self.snapshot(), Ok(()))
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use alloc::boxed::Box;
    use core::cell::RefCell;

    use axfs_ng_vfs::{MetadataChange, Mountpoint, MutationCredentials};

    use super::*;
    use crate::fs::memory::tests::BEFORE_NAMESPACE_CHANGE;

    type DirectorySizeReadHook = Box<dyn FnOnce(&MemoryNode)>;

    std::thread_local! {
        static BEFORE_DIRECTORY_SIZE_READ: RefCell<Option<DirectorySizeReadHook>> =
            const { RefCell::new(None) };
    }

    pub(super) fn before_directory_size_read(node: &MemoryNode) {
        let hook = BEFORE_DIRECTORY_SIZE_READ.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook(node);
        }
    }

    #[test]
    fn directory_metadata_allows_lookup_and_duplicate_wrapper_retirement() {
        use alloc::rc::Rc;
        use core::cell::Cell;

        struct ResetHook;
        impl Drop for ResetHook {
            fn drop(&mut self) {
                BEFORE_DIRECTORY_SIZE_READ.with(|slot| drop(slot.borrow_mut().take()));
            }
        }
        let _reset = ResetHook;
        let credentials = MutationCredentials::root();
        let filesystem = MemoryFs::new();
        let root = Mountpoint::new_root(&filesystem).root_location();
        let directory = root
            .create(
                "parent",
                NodeType::Directory,
                NodePermission::from_bits_truncate(0o755),
                0,
                0,
                &credentials,
            )
            .unwrap();
        let child = directory
            .create(
                "child",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
                &credentials,
            )
            .unwrap();
        let before = directory.metadata().unwrap();
        let progressed = Rc::new(Cell::new(false));
        let completed = progressed.clone();
        let parent = root.clone();
        BEFORE_DIRECTORY_SIZE_READ.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(move |node| {
                // This lock-order assertion protects progress, not an incidental
                // field layout. Check before running either conflicting real path,
                // so restoring metadata -> entries fails instead of spinning forever.
                let metadata = node.inode.metadata.try_lock_irqsave();
                assert!(
                    metadata.is_some(),
                    "directory size read must not overlap the inode metadata lock"
                );
                drop(metadata);

                // Actual ordinary lookup: P.entries -> fs.inodes.
                let found = node.lookup("child").unwrap();
                assert_eq!(found.inode(), child.inode());
                // Native parent lookup produces a legitimate second P wrapper,
                // as a cold-cache candidate would. Its final drop must execute
                // MemoryNode::drop -> release_inode: fs.inodes -> P.metadata.
                let candidate = parent
                    .entry()
                    .as_dir()
                    .unwrap()
                    .inner()
                    .lookup("parent")
                    .unwrap();
                let candidate_node = candidate.downcast::<MemoryNode>().unwrap();
                assert_eq!(candidate_node.inode.ino, node.inode.ino);
                assert!(!core::ptr::eq(candidate_node.as_ref(), node));
                let retired = Arc::downgrade(&candidate_node);
                drop(candidate_node);
                drop(candidate);
                assert!(
                    retired.upgrade().is_none(),
                    "duplicate wrapper must be retired"
                );
                drop(found);
                completed.set(true);
            }));
        });
        directory
            .change_metadata(
                MetadataChange::Chmod(NodePermission::from_bits_truncate(0o750)),
                &credentials,
            )
            .unwrap();
        assert!(
            progressed.get(),
            "lookup and wrapper retirement must complete"
        );
        let after = directory.metadata().unwrap();
        assert_eq!(after.mode.bits(), 0o750);
        assert_eq!(
            (after.inode, after.uid, after.gid, after.nlink, after.size),
            (before.inode, before.uid, before.gid, before.nlink, 3)
        );
        assert_eq!(before.size, 3);
        assert_eq!(
            root.lookup_no_follow("parent").unwrap().entry().inode(),
            after.inode
        );
        assert!(directory.lookup_no_follow("child").is_ok());
        BEFORE_DIRECTORY_SIZE_READ.with(|slot| assert!(slot.borrow().is_none()));
    }

    #[test]
    fn metadata_attempt_rechecks_revoked_owner_after_write_preparation() {
        struct ResetHook;
        impl Drop for ResetHook {
            fn drop(&mut self) {
                BEFORE_NAMESPACE_CHANGE.with(|slot| drop(slot.borrow_mut().take()));
            }
        }
        let _reset = ResetHook;
        let privileged = MutationCredentials::root();
        let credentials = MutationCredentials {
            fsuid: 1000,
            fsgid: 1000,
            supplementary_gids: &[],
            cap_dac_override: false,
            cap_dac_read_search: false,
            cap_fowner: false,
            cap_mknod: false,
            cap_chown: false,
            cap_fsetid: false,
        };
        let filesystem = MemoryFs::new();
        let root = Mountpoint::new_root(&filesystem).root_location();
        let input = root
            .create(
                "input",
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0o640),
                1000,
                1000,
                &MutationCredentials {
                    fsuid: 1000,
                    fsgid: 1000,
                    ..privileged
                },
            )
            .unwrap();
        let alias = root.link("alias", &input, &privileged).unwrap();
        let revoked = alias.clone();
        BEFORE_NAMESPACE_CHANGE.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(move || {
                // The real request/write/admission already exist, but its
                // owner attempt has not started. Mutate the SAME real inode.
                revoked
                    .change_metadata(
                        MetadataChange::Chown {
                            uid: Some(2000),
                            gid: None,
                        },
                        &privileged,
                    )
                    .unwrap();
            }));
        });
        assert_eq!(
            input.change_metadata(
                MetadataChange::Chmod(NodePermission::from_bits_truncate(0o777)),
                &credentials
            ),
            Err(VfsError::OperationNotPermitted),
            "preparation cannot preserve authority revoked before the real attempt"
        );
        for location in [&input, &alias] {
            let metadata = location.metadata().unwrap();
            assert_eq!(
                (metadata.uid, metadata.gid, metadata.mode.bits()),
                (2000, 1000, 0o640)
            );
        }
        BEFORE_NAMESPACE_CHANGE.with(|slot| assert!(slot.borrow().is_none()));
    }
}
