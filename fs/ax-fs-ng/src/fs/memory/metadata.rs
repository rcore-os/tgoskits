//! Metadata attempts share namespace exclusion and the real inode metadata.

use axfs_ng_vfs::{
    AuthorizedMetadataChange, LockedMetadataAttempt, MetadataRequest, NamespaceCompletion,
    NamespaceOutcome,
};

use super::*;

struct MemoryMetadataAttempt<'a> {
    node: &'a MemoryNode,
    metadata: &'a mut Metadata,
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
            let mut metadata = self.inode.metadata.lock_irqsave();
            let mut attempt = MemoryMetadataAttempt {
                node: self,
                metadata: &mut metadata,
            };
            request.execute(&mut attempt)
        };
        // Metadata is not a namespace publication and does not imply DIRSYNC.
        NamespaceCompletion::new(outcome, None)
    }
}

impl MemoryMetadataAttempt<'_> {
    fn snapshot(&self) -> Metadata {
        let mut metadata = self.metadata.clone();
        metadata.size = match &self.node.inode.content {
            NodeContent::File(file) => file.length.load(AtomicOrdering::Acquire),
            NodeContent::Dir(directory) => directory.entries.lock_irqsave().len() as u64,
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

    use axfs_ng_vfs::{MetadataChange, Mountpoint, MutationCredentials};

    use super::*;
    use crate::fs::memory::tests::BEFORE_NAMESPACE_CHANGE;

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
