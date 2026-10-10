use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::btree_map::BTreeMap,
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::any::Any;

use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, DirectoryCursor, FileNode, FilesystemOps,
    Metadata, MetadataUpdate, NodeOps, NodePermission, NodeType, Reference, RenameOptions,
    VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT, MAX_NAME_LEN},
};
use inherit_methods_macro::inherit_methods;

use super::{DirMaker, NodeOpsMux, SimpleFs, SimpleFsNode};
use crate::pseudofs::NodeOpsMuxTy;

/// Describes whether a registered directory entry may be retained by the VFS
/// dentry cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachePolicy {
    /// The entry has stable identity and may be shared between lookups.
    Shared,
    /// The entry is recreated for every lookup and must not be cached.
    PerLookup,
}

/// A validated path relative to one pseudofs root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodePath {
    components: Vec<String>,
}

impl NodePath {
    /// Parses a path relative to a filesystem root.
    pub fn new(path: &str) -> VfsResult<Self> {
        if path.is_empty() || path.starts_with('/') || path.ends_with('/') {
            return Err(VfsError::InvalidInput);
        }

        let mut components = Vec::new();
        for component in path.split('/') {
            if component.is_empty() || component == DOT || component == DOTDOT {
                return Err(VfsError::InvalidInput);
            }
            if component.contains('\0') {
                return Err(VfsError::InvalidInput);
            }
            if component.len() > MAX_NAME_LEN {
                return Err(VfsError::NameTooLong);
            }
            components.push(component.to_owned());
        }
        Ok(Self { components })
    }
}

impl TryFrom<&str> for NodePath {
    type Error = VfsError;

    fn try_from(path: &str) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl TryFrom<String> for NodePath {
    type Error = VfsError;

    fn try_from(path: String) -> Result<Self, Self::Error> {
        Self::new(&path)
    }
}

/// Registers one or more nodes in a [`NodeRegistry`].
pub trait FsNodeRegistration {
    /// Adds this node or node group to the supplied build-time registry.
    fn register(&self, registry: &mut NodeRegistry) -> VfsResult<()>;
}

impl<F> FsNodeRegistration for F
where
    F: Fn(&mut NodeRegistry) -> VfsResult<()>,
{
    fn register(&self, registry: &mut NodeRegistry) -> VfsResult<()> {
        self(registry)
    }
}

/// Operations for a simple directory.
pub trait SimpleDirOps: Send + Sync + 'static {
    /// Get the names of all children in the directory.
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a>;
    /// Look up a child directory or file by name.
    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux>;

    /// Check if the directory is cacheable.
    ///
    /// See [`DirNodeOps::is_cacheable`].
    fn is_cacheable(&self) -> bool {
        true
    }

    /// Check whether one child has a stable directory entry.
    fn is_cacheable_child(&self, _name: &str) -> bool {
        self.is_cacheable()
    }

    /// Combines two directories into one.
    fn chain<N: SimpleDirOps>(self, other: N) -> ChainedDirOps<Self, N>
    where
        Self: Sized,
    {
        ChainedDirOps(self, other)
    }
}

impl SimpleDirOps for DirMapping {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.map.keys().map(|s| s.as_str().into()))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        self.map
            .get(name)
            .map(|entry| match &entry.ops {
                NodeOpsMuxTy::Static(ops) => ops.clone(),
                NodeOpsMuxTy::Dynamic(maker) => maker(),
            })
            .ok_or(VfsError::NotFound)
    }

    fn is_cacheable(&self) -> bool {
        self.cacheable
    }

    fn is_cacheable_child(&self, name: &str) -> bool {
        self.cacheable
            && self
                .map
                .get(name)
                .is_none_or(|entry| entry.cache_policy == CachePolicy::Shared)
    }
}

/// A mapping of directory names to entries.
pub struct DirMapping {
    map: BTreeMap<String, DirMappingEntry>,
    cacheable: bool,
}

struct DirMappingEntry {
    ops: NodeOpsMuxTy,
    cache_policy: CachePolicy,
}

impl DirMapping {
    /// Create a new empty directory mapping.
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            cacheable: true,
        }
    }

    /// Set whether the directory is cacheable.
    pub fn set_cacheable(&mut self, cacheable: bool) {
        self.cacheable = cacheable;
    }

    /// Add a new entry to the directory mapping.
    pub fn add(&mut self, name: impl Into<String>, ops: impl Into<NodeOpsMux>) {
        self.map.insert(
            name.into(),
            DirMappingEntry {
                ops: NodeOpsMuxTy::Static(ops.into()),
                cache_policy: CachePolicy::Shared,
            },
        );
    }

    /// Add a new entry to the directory mapping, created on demand.
    pub fn add_dynamic(
        &mut self,
        name: impl Into<String>,
        maker: impl Fn() -> NodeOpsMux + Send + Sync + 'static,
    ) {
        self.add_dynamic_with_cache_policy(name, maker, CachePolicy::Shared);
    }

    fn add_dynamic_with_cache_policy(
        &mut self,
        name: impl Into<String>,
        maker: impl Fn() -> NodeOpsMux + Send + Sync + 'static,
        cache_policy: CachePolicy,
    ) {
        self.map.insert(
            name.into(),
            DirMappingEntry {
                ops: NodeOpsMuxTy::Dynamic(Box::new(maker)),
                cache_policy,
            },
        );
    }
}

impl Default for DirMapping {
    fn default() -> Self {
        Self::new()
    }
}

enum RegistrationEntry {
    Node(NodeOpsMux),
    Dynamic {
        maker: Box<dyn Fn() -> NodeOpsMux + Send + Sync>,
        cache_policy: CachePolicy,
    },
    Directory(RegistrationDir),
    OpaqueDirectory(DirMaker),
}

#[derive(Default)]
struct RegistrationDir {
    entries: BTreeMap<String, RegistrationEntry>,
}

/// Build-time registry for entries rooted at one [`SimpleFs`].
pub struct NodeRegistry {
    root: RegistrationDir,
}

impl NodeRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            root: RegistrationDir::default(),
        }
    }

    /// Registers a stable node at a relative path.
    pub fn node<P>(&mut self, path: P, ops: impl Into<NodeOpsMux>) -> VfsResult<()>
    where
        P: TryInto<NodePath, Error = VfsError>,
    {
        self.insert(path.try_into()?, RegistrationEntry::Node(ops.into()))
    }

    /// Registers a node factory with an explicit cache policy.
    pub fn dynamic_node<P>(
        &mut self,
        path: P,
        cache_policy: CachePolicy,
        maker: impl Fn() -> NodeOpsMux + Send + Sync + 'static,
    ) -> VfsResult<()>
    where
        P: TryInto<NodePath, Error = VfsError>,
    {
        self.insert(
            path.try_into()?,
            RegistrationEntry::Dynamic {
                maker: Box::new(maker),
                cache_policy,
            },
        )
    }

    /// Registers a directory provider whose children are resolved by the
    /// supplied directory operations.
    pub fn directory<P>(&mut self, path: P, maker: DirMaker) -> VfsResult<()>
    where
        P: TryInto<NodePath, Error = VfsError>,
    {
        self.insert(path.try_into()?, RegistrationEntry::OpaqueDirectory(maker))
    }

    /// Reserves a stable, empty directory for a later mount.
    pub fn reserve_mountpoint<P>(&mut self, path: P) -> VfsResult<()>
    where
        P: TryInto<NodePath, Error = VfsError>,
    {
        let path = path.try_into()?;
        let (name, parents) = split_path(&path)?;
        Self::validate_parent(&self.root, parents)?;
        let parent = Self::parent_dir_mut(&mut self.root, parents)?;
        if parent.entries.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        parent.entries.insert(
            name.to_owned(),
            RegistrationEntry::Directory(RegistrationDir::default()),
        );
        Ok(())
    }

    /// Registers a node or node group implemented by a provider.
    pub fn register<T: FsNodeRegistration>(&mut self, item: &T) -> VfsResult<()> {
        item.register(self)
    }

    /// Materializes the registry into a directory mapping.
    pub(crate) fn finish_mapping(self, fs: Arc<SimpleFs>) -> VfsResult<DirMapping> {
        Self::build_mapping(self.root, fs)
    }

    /// Materializes the registry into the existing [`DirMaker`] interface.
    // This convenience entry point is intentionally retained for builders that
    // do not need to compose a dynamic root provider.
    #[cfg_attr(feature = "uvc", allow(dead_code))]
    pub fn finish(self, fs: Arc<SimpleFs>) -> VfsResult<DirMaker> {
        let root = self.finish_mapping(fs.clone())?;
        Ok(SimpleDir::new_maker(fs, Arc::new(root)))
    }

    fn insert(&mut self, path: NodePath, entry: RegistrationEntry) -> VfsResult<()> {
        let (name, parents) = split_path(&path)?;
        Self::validate_parent(&self.root, parents)?;
        let parent = Self::parent_dir_mut(&mut self.root, parents)?;
        if parent.entries.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        parent.entries.insert(name.to_owned(), entry);
        Ok(())
    }

    fn validate_parent(dir: &RegistrationDir, components: &[String]) -> VfsResult<()> {
        let Some((component, rest)) = components.split_first() else {
            return Ok(());
        };
        match dir.entries.get(component) {
            None => Ok(()),
            Some(RegistrationEntry::Directory(child)) => Self::validate_parent(child, rest),
            Some(_) => Err(VfsError::NotADirectory),
        }
    }

    fn parent_dir_mut<'a>(
        dir: &'a mut RegistrationDir,
        components: &[String],
    ) -> VfsResult<&'a mut RegistrationDir> {
        let Some((component, rest)) = components.split_first() else {
            return Ok(dir);
        };
        if !dir.entries.contains_key(component) {
            dir.entries.insert(
                component.clone(),
                RegistrationEntry::Directory(RegistrationDir::default()),
            );
        }
        match dir.entries.get_mut(component) {
            Some(RegistrationEntry::Directory(child)) => Self::parent_dir_mut(child, rest),
            Some(_) => Err(VfsError::NotADirectory),
            None => unreachable!("registration parent was inserted above"),
        }
    }

    fn build_mapping(dir: RegistrationDir, fs: Arc<SimpleFs>) -> VfsResult<DirMapping> {
        let mut mapping = DirMapping::new();
        for (name, entry) in dir.entries {
            match entry {
                RegistrationEntry::Node(ops) => mapping.add(name, ops),
                RegistrationEntry::Dynamic {
                    maker,
                    cache_policy,
                } => mapping.add_dynamic_with_cache_policy(name, maker, cache_policy),
                RegistrationEntry::Directory(child) => {
                    let child = Self::build_mapping(child, fs.clone())?;
                    mapping.add(
                        name,
                        SimpleDir::new_maker(fs.clone(), Arc::new(child)),
                    );
                }
                RegistrationEntry::OpaqueDirectory(maker) => mapping.add(name, maker),
            }
        }
        Ok(mapping)
    }
}

impl Default for NodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn split_path(path: &NodePath) -> VfsResult<(&str, &[String])> {
    let (name, parents) = path
        .components
        .split_last()
        .ok_or(VfsError::InvalidInput)?;
    Ok((name.as_str(), parents))
}

/// Directory created by [`SimpleDirOps::chain`].
pub struct ChainedDirOps<A, B>(A, B);

impl<A: SimpleDirOps, B: SimpleDirOps> SimpleDirOps for ChainedDirOps<A, B> {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.0.child_names().chain(self.1.child_names()))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match self.0.lookup_child(name) {
            Ok(ops) => Ok(ops),
            Err(VfsError::NotFound) => self.1.lookup_child(name),
            Err(e) => Err(e),
        }
    }

    fn is_cacheable(&self) -> bool {
        // TODO: If one of the ops is not cacheable while the other is, the
        // behavior is undefined.
        self.0.is_cacheable() && self.1.is_cacheable()
    }
}

/// Simple directory.
pub struct SimpleDir<O> {
    node: SimpleFsNode,
    this: WeakDirEntry,
    ops: Arc<O>,
}

impl<O: SimpleDirOps> SimpleDir<O> {
    fn new(node: SimpleFsNode, ops: Arc<O>, this: WeakDirEntry) -> Arc<Self> {
        Arc::new(Self { node, this, ops })
    }

    /// Create a [`DirMaker`] from given directory operations.
    pub fn new_maker(fs: Arc<SimpleFs>, ops: Arc<O>) -> DirMaker {
        Arc::new(move |this| {
            SimpleDir::new(
                SimpleFsNode::new(
                    fs.clone(),
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(0o755),
                ),
                ops.clone(),
                this,
            )
        })
    }
}

#[inherit_methods(from = "self.node")]
impl<O: SimpleDirOps> NodeOps for SimpleDir<O> {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl<O: SimpleDirOps> DirNodeOps for SimpleDir<O> {
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let children = [DOT, DOTDOT]
            .into_iter()
            .map(Cow::Borrowed)
            .chain(self.ops.child_names());

        let this_entry = self.this.upgrade().unwrap();
        let this_dir = this_entry.as_dir()?;

        let mut count = 0;
        for (i, name) in children.enumerate().skip(cursor.offset() as usize) {
            let metadata = match name.as_ref() {
                DOT => this_entry.metadata(),
                DOTDOT => this_entry
                    .parent()
                    .map_or_else(|| this_entry.metadata(), |parent| parent.metadata()),
                other => {
                    let entry = this_dir.lookup(other)?;
                    entry.metadata()
                }
            }?;
            if !sink.accept(
                name.as_bytes(),
                metadata.inode,
                metadata.node_type,
                DirectoryCursor::new(i as u64 + 1),
            ) {
                break;
            }
            count += 1;
        }

        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        let ops = self.ops.lookup_child(name)?;
        let reference = Reference::new(self.this.upgrade(), name.to_owned());
        Ok(match ops {
            NodeOpsMux::Dir(maker) => {
                DirEntry::new_dir(|this| DirNode::new(maker(this)), reference)
            }
            NodeOpsMux::File(ops) => {
                let node_type = ops.metadata()?.node_type;
                DirEntry::new_file(FileNode::new(ops.clone()), node_type, reference)
            }
        })
    }

    fn is_cacheable(&self) -> bool {
        self.ops.is_cacheable()
    }

    fn is_cacheable_child(&self, name: &str) -> bool {
        self.ops.is_cacheable_child(name)
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::OperationNotPermitted)
    }

    fn create_symlink(
        &self,
        _name: &str,
        _target: &str,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::OperationNotPermitted)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::OperationNotPermitted)
    }

    fn unlink(&self, _name: &str, _is_dir: bool) -> VfsResult<()> {
        Err(VfsError::OperationNotPermitted)
    }

    fn rename(
        &self,
        _src_name: &str,
        _dst_dir: &DirNode,
        _dst_name: &str,
        _options: RenameOptions,
    ) -> VfsResult<()> {
        Err(VfsError::OperationNotPermitted)
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    fn opaque_directory() -> RegistrationEntry {
        let maker: DirMaker = Arc::new(|_| panic!("test directory must not be opened"));
        RegistrationEntry::OpaqueDirectory(maker)
    }

    #[test]
    fn node_path_rejects_non_relative_or_non_normal_components() {
        assert!(NodePath::new("dev/null").is_ok());
        for path in ["", "/dev/null", "dev/", "dev//null", "dev/./null", "dev/../null"] {
            assert_eq!(NodePath::new(path), Err(VfsError::InvalidInput), "{path:?}");
        }
        assert_eq!(NodePath::new("dev/\0null"), Err(VfsError::InvalidInput));
        assert_eq!(NodePath::new(&"x".repeat(MAX_NAME_LEN + 1)), Err(VfsError::NameTooLong));
    }

    #[test]
    fn registry_creates_parents_and_rejects_conflicts() {
        let mut registry = NodeRegistry::new();
        registry.reserve_mountpoint("bus/usb").unwrap();
        assert!(matches!(registry.root.entries.get("bus"), Some(RegistrationEntry::Directory(_))));
        assert_eq!(registry.reserve_mountpoint("bus/usb"), Err(VfsError::AlreadyExists));
        assert_eq!(
            registry.insert(NodePath::new("bus/usb").unwrap(), opaque_directory()),
            Err(VfsError::AlreadyExists)
        );
        registry
            .insert(NodePath::new("bus/usb/host0").unwrap(), opaque_directory())
            .unwrap();

        let mut opaque_parent = NodeRegistry::new();
        opaque_parent
            .insert(NodePath::new("dev").unwrap(), opaque_directory())
            .unwrap();
        assert_eq!(
            opaque_parent.insert(NodePath::new("dev/null").unwrap(), opaque_directory()),
            Err(VfsError::NotADirectory)
        );
    }

    #[test]
    fn invalid_registration_does_not_mutate_tree() {
        let mut registry = NodeRegistry::new();
        assert_eq!(
            registry.reserve_mountpoint("bad//path"),
            Err(VfsError::InvalidInput)
        );
        assert!(registry.root.entries.is_empty());
    }

    #[test]
    fn dynamic_cache_policy_is_explicit() {
        let mut mapping = DirMapping::new();
        mapping.add_dynamic_with_cache_policy(
            "tun",
            || NodeOpsMux::Dir(Arc::new(|_| panic!("test node must not be opened"))),
            CachePolicy::PerLookup,
        );
        assert!(!mapping.is_cacheable_child("tun"));

        mapping.add_dynamic_with_cache_policy(
            "stable",
            || NodeOpsMux::Dir(Arc::new(|_| panic!("test node must not be opened"))),
            CachePolicy::Shared,
        );
        assert!(mapping.is_cacheable_child("stable"));
    }

    #[test]
    fn registry_materializes_static_aliases_for_lookup_and_listing() {
        let maker: DirMaker = Arc::new(|_| panic!("test directory must not be opened"));
        let mut registry = NodeRegistry::new();
        registry.directory("primary", maker.clone()).unwrap();
        registry.directory("alias", maker.clone()).unwrap();

        let mut fs_ref = None;
        let _filesystem = SimpleFs::try_new_with("test".into(), 0, |fs| {
            fs_ref = Some(fs.clone());
            Ok(SimpleDir::new_maker(fs, Arc::new(DirMapping::new())))
        })
        .unwrap();
        let mapping = registry.finish_mapping(fs_ref.unwrap()).unwrap();

        let names: Vec<_> = mapping
            .child_names()
            .map(|name| name.into_owned())
            .collect();
        assert_eq!(names, ["alias", "primary"]);
        let NodeOpsMux::Dir(primary) = mapping.lookup_child("primary").unwrap() else {
            panic!("registered directory is not a directory");
        };
        assert!(Arc::ptr_eq(&primary, &maker));
        let NodeOpsMux::Dir(alias) = mapping.lookup_child("alias").unwrap() else {
            panic!("registered alias is not a directory");
        };
        assert!(Arc::ptr_eq(&alias, &maker));
    }
}
