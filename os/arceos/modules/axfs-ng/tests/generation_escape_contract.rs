//! Public-boundary contract for generation-safe raw filesystem locations.

const HANDLE: &str = include_str!("../src/file/handle.rs");
const OPEN: &str = include_str!("../src/file/open.rs");
const CACHE: &str = include_str!("../src/file/cache/handle.rs");
const CACHE_SHARED: &str = include_str!("../src/file/cache/shared.rs");
const LOCATION: &str = include_str!("../src/file/location.rs");
const CONTEXT: &str = include_str!("../src/fs_core/context/state.rs");

#[test]
fn raw_locations_cannot_construct_unmanaged_axfs_handles() {
    assert!(
        HANDLE.contains("Direct(UnmanagedLocation)"),
        "direct backends must carry a checked unmanaged-location capability"
    );
    assert!(
        OPEN.contains("pub fn open_loc(&self, loc: UnmanagedLocation)"),
        "open_loc must not accept an arbitrary raw Location"
    );
    assert!(
        CACHE.contains("pub fn get_or_create(location: UnmanagedLocation)"),
        "the public cache constructor must not accept an arbitrary raw Location"
    );
    assert!(
        CONTEXT.contains("pub fn new(root_dir: UnmanagedLocation)"),
        "an unmanaged FsContext must require an explicit checked capability"
    );
    assert!(
        CONTEXT.contains("pub fn open_cached_location(&self, location: FileLocation)"),
        "a context must accept only a location carrying its original generation authority"
    );
    assert!(
        CONTEXT.contains("let lease = self.open_handle()?.ok_or(VfsError::BadState)?;"),
        "promoting a managed location into a cache must acquire a counted handle lease"
    );
    assert!(
        CONTEXT.contains(".validate_operation(&operation)"),
        "cache promotion must authorize the location with the new handle's operation lease"
    );
    assert!(
        CONTEXT.contains("CachedFile::get_or_create_generation_bound(location, lease)"),
        "the counted handle lease must be transferred into the promoted cache"
    );
    assert!(
        CACHE.contains("authority: Option<ManagedCachedFileAuthority>")
            && CACHE.contains("struct ManagedCachedFileAuthority")
            && CACHE.contains("lease: FsOpenHandleLease"),
        "generation-bound cached-file clones must retain their shared counted lease"
    );
    assert!(
        HANDLE.contains("lease: FsOpenHandleLease"),
        "generation-bound direct-backend clones must retain their shared counted lease"
    );
    assert!(
        !CACHE_SHARED.contains("FsOpenHandleLease"),
        "inode-global page-cache state must remain lease-free to avoid a runtime ownership cycle"
    );
    assert!(
        OPEN.contains("pub fn with_fs_context<T>("),
        "directory-relative contexts must be authorized by the opened directory lease"
    );
    assert!(
        LOCATION.contains("pub enum FileLocation"),
        "managed and unmanaged location authority must remain explicit in the type system"
    );
    assert!(
        !LOCATION.contains("pub fn location(&self)"),
        "a public raw-location borrow would let callers bypass generation validation"
    );
    assert!(
        !HANDLE.contains("Direct(Location)"),
        "the old raw direct-backend variant bypasses generation leases"
    );
    assert!(
        !HANDLE.contains("pub fn new(inner: FileBackend"),
        "a public generic File constructor can discard a managed backend's open-handle lease"
    );
    assert!(
        HANDLE.contains("pub fn from_unmanaged(inner: FileBackend"),
        "synthetic filesystem construction must use the explicit unmanaged API"
    );
    assert!(
        !CONTEXT.contains("open_cached_location(&self, location: Location)"),
        "raw locations must not be relabelled with the consuming context generation"
    );
    assert!(
        !CONTEXT.contains("pub fn with_current_dir(&self, current_dir: Location)"),
        "raw directories must not be relabelled with the consuming context generation"
    );
    assert!(
        CONTEXT.contains("pub fn set_current_dir(&mut self, current_dir: FileLocation)"),
        "cwd updates must consume typed location authority"
    );
    assert!(
        CONTEXT.contains("pub fn reset_root(&mut self, root_dir: FileLocation)"),
        "root updates must consume typed location authority"
    );
    assert!(
        !CONTEXT.contains("pub fn set_current_dir(&mut self, current_dir: Location)"),
        "raw locations must not be installed as the cwd"
    );
    assert!(
        !CONTEXT.contains("pub fn reset_root(&mut self, root_dir: Location)"),
        "raw locations must not be installed as the root"
    );
}
