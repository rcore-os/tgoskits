//! Public-boundary contract for generation-scoped file operations.

const HANDLE: &str = include_str!("../src/file/handle.rs");
const CACHE: &str = include_str!("../src/file/cache/handle.rs");
const LOCATION: &str = include_str!("../src/file/location.rs");
const OPEN: &str = include_str!("../src/file/open.rs");
const OPERATION: &str = include_str!("../src/file/operation.rs");
const CONTEXT_STATE: &str = include_str!("../src/fs_core/context/state.rs");
const CONTEXT_OPERATION: &str = include_str!("../src/fs_core/context/operation.rs");
const API: &str = include_str!("../src/api.rs");
const UNIX_NAMESPACE: &str = include_str!("../../axruntime/src/unix_ns.rs");

#[test]
fn unix_namespace_does_not_borrow_a_raw_vfs_location() {
    assert!(
        HANDLE.contains("pub fn with_operation<T>("),
        "file metadata and user data must be exposed through a scoped operation capability"
    );
    assert!(
        UNIX_NAMESPACE.contains(".with_operation("),
        "the unix namespace must retain the file generation lease for its complete operation"
    );
    assert!(
        !UNIX_NAMESPACE.contains(".location()"),
        "the unix namespace must not clone or retain an unscoped raw Location"
    );
    assert!(
        HANDLE.matches("pub fn with_operation<T>(").count() >= 2,
        "both File and FileBackend must expose the restricted operation boundary"
    );
    assert!(
        CACHE.contains("pub fn with_operation<T>("),
        "CachedFile must expose the same restricted operation boundary"
    );
    assert!(
        LOCATION.contains("pub fn with_operation<T>(")
            && OPEN.contains("pub fn with_operation<T>("),
        "typed locations and opened directories need a restricted replacement for raw callbacks"
    );
    assert!(
        HANDLE.contains(
            "impl for<'operation> FnOnce(LocationOperationView<'operation>) -> VfsResult<T>"
        ) && CACHE.contains(
            "impl for<'operation> FnOnce(LocationOperationView<'operation>) -> VfsResult<T>"
        ),
        "the operation callback must be higher-ranked so its view cannot escape"
    );
    assert!(
        OPERATION.contains("pub struct LocationOperationView<'operation>"),
        "the scoped operation authority must have a dedicated public type"
    );
    assert!(
        !HANDLE.contains("pub fn location(")
            && !CACHE.contains("pub fn location(")
            && !LOCATION.contains("pub fn with_location")
            && !OPEN.contains("pub fn with_location"),
        "public file and directory APIs must not expose legacy raw locations"
    );
    assert!(
        OPERATION.contains("_managed_lease: Option<&'operation FsOperationLease>")
            && OPERATION.contains("pub(super) const fn managed(")
            && OPERATION.contains("pub(super) const fn unmanaged("),
        "managed views must borrow their exact operation lease and use a distinct constructor"
    );
    assert!(
        !OPERATION.contains("pub fn location(")
            && !OPERATION.contains("impl Deref")
            && !OPERATION.contains("AsRef<Location>"),
        "the restricted view must not expose or reconstruct its raw Location"
    );
    assert!(
        !LOCATION.contains("impl From<Location> for FileLocation")
            && !LOCATION.contains("impl TryFrom<Location> for FileLocation")
            && !LOCATION.contains("pub fn from_access")
            && !LOCATION.contains("pub fn from_handle"),
        "an escaped raw Location must not be relabelled as the current managed generation"
    );
    for raw_api in [
        "pub fn resolve(&self",
        "pub fn resolve_no_follow(&self",
        "pub fn resolve_parent<'a>",
        "pub fn resolve_nonexistent<'a>",
        "pub fn root_dir(&self) -> &Location",
        "pub fn current_dir(&self) -> &Location",
    ] {
        assert!(
            !CONTEXT_STATE.contains(raw_api),
            "FsContext still exposes an operation-unscoped raw Location through `{raw_api}`"
        );
    }
    assert!(
        CONTEXT_STATE.contains("pub fn with_namespace_operation<T>(")
            && CONTEXT_STATE.contains("impl for<'operation> FnOnce(")
            && CONTEXT_STATE.contains("FsNamespaceOperationView<'operation>"),
        "FsContext needs a higher-ranked namespace-operation boundary that retains its exact lease"
    );
    assert!(
        CONTEXT_OPERATION.contains("operation: Option<&'operation FsOperationLease>")
            && CONTEXT_OPERATION.contains("LocationOperationView::managed_owned")
            && CONTEXT_OPERATION.contains("pub fn resolve_path(")
            && CONTEXT_OPERATION.contains("pub fn parent_for_create<'a>("),
        "namespace resolution must return a restricted view borrowing the exact admitted lease"
    );
    assert!(
        !CONTEXT_OPERATION.contains("pub fn location(")
            && !CONTEXT_OPERATION.contains("-> VfsResult<Location>")
            && !CONTEXT_OPERATION.contains("-> &Location"),
        "the namespace-operation view must not expose a raw resolved Location"
    );
    assert!(
        OPERATION.contains("pub fn authorize_location<'view>(")
            && OPERATION.contains(".validate_operation(operation)"),
        "composite operations must reuse their exact lease when authorizing another retained \
         location"
    );
}

#[test]
fn current_directory_query_keeps_a_generation_operation_lease() {
    assert!(
        API.contains("with_namespace_operation(|namespace|"),
        "the public current-directory query must be rejected after its generation freezes"
    );
    assert!(
        API.contains("namespace.current_dir().absolute_path()"),
        "the current directory must be read through the operation-scoped namespace view"
    );
}

#[test]
fn file_drop_does_not_readmit_metadata_work_after_its_generation_operation_begins() {
    let drop_impl = HANDLE
        .split("impl Drop for File")
        .nth(1)
        .expect("File must retain an explicit Drop implementation");

    assert!(
        !drop_impl.contains("self.with_operation("),
        "File::drop must reuse its already-admitted operation; a nested admission can fail if \
         freezing starts between the two checks"
    );
    assert!(
        drop_impl.contains("operation_lease.as_ref()")
            && drop_impl.contains("LocationOperationView::managed("),
        "File::drop must bind metadata writeback to the exact operation admitted before freeze"
    );
}

#[test]
fn directory_relative_context_operations_cannot_escape_or_readmit() {
    assert!(
        CONTEXT_OPERATION.contains("pub struct FsContextOperationView<'operation>")
            && CONTEXT_OPERATION.contains("operation: Option<&'operation FsOperationLease>"),
        "the scoped context must borrow the exact admitted generation operation"
    );
    assert!(
        OPEN.contains(
            "impl for<'operation> FnOnce(FsContextOperationView<'operation>) -> VfsResult<T>"
        ) && !OPEN.contains("impl FnOnce(&mut FsContext) -> VfsResult<T>"),
        "OpenedDirectory callbacks must receive only the non-escaping scoped context view"
    );
    assert!(
        CONTEXT_OPERATION.contains("pub fn with_namespace_operation<T>(")
            && CONTEXT_OPERATION.contains("metadata_during(path.as_ref(), self.operation)")
            && CONTEXT_OPERATION.contains("options.open_scoped("),
        "nested namespace, metadata, and open operations must reuse the existing lease"
    );
    assert!(
        !CONTEXT_OPERATION.contains("impl Deref for FsContextOperationView")
            && !CONTEXT_OPERATION.contains("AsRef<FsContext> for FsContextOperationView")
            && !CONTEXT_OPERATION.contains("pub fn context("),
        "the scoped view must not expose the ordinary FsContext admission surface"
    );
    assert!(
        CONTEXT_STATE.contains("pub fn metadata(&self")
            && CONTEXT_STATE.contains("let operation = self.begin_operation()?;")
            && CONTEXT_STATE.contains("pub fn with_operation_scope<T>("),
        "ordinary FsContext APIs must keep independent admission while composites opt into the \
         scoped API"
    );
}
