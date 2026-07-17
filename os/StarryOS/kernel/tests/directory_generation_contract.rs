//! Source-level contract for generation-bound Starry directory descriptors.

const FILE_FS: &str = include_str!("../src/file/fs.rs");

#[test]
fn directory_descriptor_retains_the_axfs_generation_lease() {
    assert!(
        FILE_FS.contains("inner: OpenedDirectory"),
        "Starry directory descriptors must own ax-fs-ng's generation-bound directory"
    );
    assert!(
        FILE_FS.contains("directory.with_fs_context(&fs, f)"),
        "dirfd-relative operations must validate the directory against the exact fs context"
    );
    assert!(
        FILE_FS.contains("pub fn with_operation<T>(")
            && FILE_FS.contains("LocationOperationView<'operation>"),
        "directory operations must use a non-escaping view while the generation lease is retained"
    );
    assert!(
        !FILE_FS.contains("OpenedDirectoryAccess"),
        "directory descriptors must not expose a raw-location deref guard"
    );
    assert!(
        !FILE_FS.contains("pub fn inner(&self) -> &Location"),
        "a bare Location must not escape a generation-bound directory descriptor"
    );
}
