//! Source-level contract for generation-safe Starry executable locations.

const FILE_FS: &str = include_str!("../src/file/fs.rs");
const ENTRY: &str = include_str!("../src/entry.rs");
const EXECVE: &str = include_str!("../src/syscall/task/execve.rs");
const LOADER: &str = include_str!("../src/mm/loader.rs");
const XATTR: &str = include_str!("../src/syscall/fs/xattr.rs");

#[test]
fn executable_locations_retain_axfs_generation_authority() {
    assert!(
        FILE_FS.contains("File(FileLocation)"),
        "dirfd resolution must not erase the filesystem-generation capability"
    );
    assert!(
        EXECVE.contains("resolve_file_location"),
        "exec path resolution must produce a generation-aware location"
    );
    assert!(
        LOADER.contains("loc: FileLocation"),
        "the ELF loader must require generation authority with its location"
    );
    assert!(
        LOADER.contains("open_cached_location(loc)"),
        "the loader must pass the original location authority into ax-fs-ng"
    );
    assert!(
        !FILE_FS.contains("File(Location)"),
        "a raw VFS location must not escape dirfd resolution"
    );
    assert!(
        !ENTRY.contains("loc.location()")
            && !EXECVE.contains("loc.location()")
            && !LOADER.contains("loc.location()"),
        "Starry must inspect executable locations only inside a generation operation scope"
    );
    assert!(
        XATTR.contains("LocationOperationView<'operation>"),
        "xattr callbacks must receive a non-escaping operation view"
    );
    assert!(
        !XATTR.contains("axfs_ng_vfs::Location")
            && XATTR.contains("overlay::visible_user_data")
            && XATTR.contains("overlay::writable_user_data"),
        "xattr overlay handling must stay typed and must not reconstruct a raw Location"
    );
}
