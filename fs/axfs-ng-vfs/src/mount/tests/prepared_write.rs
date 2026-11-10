//! Dependency descriptions are fixtures; all write ownership is real VFS state.

use super::*;
use crate::PreparedWrite;

struct WriteLayer {
    this: Weak<Self>,
    depth: u32,
    roots: Vec<Location>,
}

struct WriteLayerRoot {
    filesystem: Arc<WriteLayer>,
    node: MockNode,
}

impl FilesystemOps for WriteLayer {
    fn name(&self) -> &str {
        "write-layer-fixture"
    }

    fn root_dir(&self) -> DirEntry {
        let node = Arc::new(WriteLayerRoot {
            filesystem: self.this.upgrade().unwrap(),
            node: MockNode,
        });
        DirEntry::new_dir(|_| DirNode::new(node), Reference::root())
    }

    fn stack_depth(&self) -> u32 {
        self.depth
    }

    fn write_backing_roots(&self) -> &[Location] {
        &self.roots
    }

    fn stat(&self) -> VfsResult<StatFs> {
        MOCK_FS.stat()
    }
}

#[inherit_methods(from = "self.node")]
impl NodeOps for WriteLayerRoot {
    fn inode(&self) -> u64;
    fn metadata(&self) -> VfsResult<Metadata>;
    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;
    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.filesystem.as_ref()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[inherit_methods(from = "self.node")]
impl DirNodeOps for WriteLayerRoot {
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize>;
    fn lookup(&self, name: &str) -> VfsResult<DirEntry>;
    fn mutate_namespace(
        &self,
        command: &crate::NamespaceMutation<'_>,
        retired: &crate::MutationRetire,
    ) -> crate::NamespaceOutcome<crate::NamespaceEffect>;
}

fn layer(depth: u32, roots: Vec<Location>) -> Location {
    let filesystem = Filesystem::new(Arc::new_cyclic(|this| WriteLayer {
        this: this.clone(),
        depth,
        roots,
    }));
    Mountpoint::new_root(&filesystem).root_location()
}

#[test]
fn prepared_write_retains_exact_mounts_across_dependencies_and_releases_failed_preparations() {
    let leaf = layer(0, vec![]);
    let host = Mountpoint::new_root(&mock_filesystem());
    let bind = |name| {
        let target = Location::new(
            host.clone(),
            make_child_dir_entry(Some(host.root.clone()), name),
        );
        target.bind_mount(&leaf, false).unwrap().root_location()
    };
    let first = bind("first");
    let second = bind("second");
    let uncovered = bind("uncovered");
    let middle = layer(1, vec![first.clone(), second.clone()]);
    let visible = layer(2, vec![middle.clone(), first.clone()]);
    let held = [&visible, &middle, &first, &second];

    let prepared = PreparedWrite::acquire(&visible).unwrap();
    for location in held {
        prepared
            .write_for(location)
            .unwrap()
            .check_location(location)
            .unwrap();
        // Counts come from real admission, not from the dependency fixture.
        // The repeated first root must not own a second lease.
        assert_eq!(location.mountpoint().writes.lock().writers, 1);
        assert_eq!(
            location.mountpoint().set_readonly(true),
            Err(VfsError::ResourceBusy)
        );
        assert!(!location.is_readonly());
    }
    assert_eq!(leaf.mountpoint().filesystem_state.writes.lock().writers, 2);
    assert_eq!(
        uncovered.mountpoint().set_filesystem_readonly(true),
        Err(VfsError::ResourceBusy)
    );
    assert_eq!(
        prepared.write_for(&uncovered).unwrap_err(),
        VfsError::CrossesDevices
    );
    assert_eq!(
        prepared.write_for(&first).unwrap().check_location(&second),
        Err(VfsError::CrossesDevices)
    );
    // The unprepared bind has no local writer, despite sharing the filesystem.
    uncovered.mountpoint().set_readonly(true).unwrap();
    assert_eq!(
        PreparedWrite::acquire(&uncovered).unwrap_err(),
        VfsError::ReadOnlyFilesystem
    );
    drop(prepared);
    for location in held {
        location.mountpoint().set_readonly(true).unwrap();
    }
    uncovered
        .mountpoint()
        .set_filesystem_readonly(true)
        .unwrap();

    for blocked in [&middle, &second] {
        for location in held {
            location
                .mountpoint()
                .set_filesystem_readonly(false)
                .unwrap();
            location.mountpoint().set_readonly(false).unwrap();
        }
        blocked.mountpoint().set_readonly(true).unwrap();
        assert_eq!(
            PreparedWrite::acquire(&visible).unwrap_err(),
            VfsError::ReadOnlyFilesystem
        );
        // Transitions succeed only if all earlier recursive admissions unwound.
        for location in held {
            location.mountpoint().set_readonly(true).unwrap();
            location.mountpoint().set_filesystem_readonly(true).unwrap();
        }
    }

    let nondecreasing = layer(1, vec![layer(1, vec![])]);
    for invalid in [layer(3, vec![leaf]), nondecreasing] {
        assert_eq!(
            PreparedWrite::acquire(&invalid).unwrap_err(),
            VfsError::InvalidInput
        );
        invalid.mountpoint().set_readonly(true).unwrap();
        invalid.mountpoint().set_filesystem_readonly(true).unwrap();
    }
}
