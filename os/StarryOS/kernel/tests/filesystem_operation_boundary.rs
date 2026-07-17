//! Starry must keep every detachable-filesystem operation generation-scoped.

use std::{fs, path::PathBuf};

const OVERLAY: &str = include_str!("../src/pseudofs/overlay.rs");
const FILE_FS: &str = include_str!("../src/file/fs.rs");

fn starry_source() -> String {
    let mut pending = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut source = String::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        {
            let path = entry.expect("Starry source entry must be readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                source.push_str(
                    &fs::read_to_string(&path).unwrap_or_else(|error| {
                        panic!("failed to read {}: {error}", path.display())
                    }),
                );
            }
        }
    }
    source
}

#[test]
fn starry_does_not_escape_raw_filesystem_locations() {
    let source = starry_source();

    for forbidden in [
        ".location()",
        ".with_location(",
        ".resolve(",
        ".resolve_no_follow(",
        ".resolve_parent(",
        ".resolve_nonexistent(",
        ".root_dir()",
    ] {
        assert!(
            !source.contains(forbidden),
            "Starry still bypasses the generation-scoped operation view through `{forbidden}`"
        );
    }
}

#[test]
fn overlay_reuses_one_generation_operation_for_composite_work() {
    assert!(
        OVERLAY.contains("authority.authorize_location(&self.0)"),
        "overlay backing locations must be authorized by the already-admitted generation operation"
    );
    assert_eq!(
        OVERLAY.matches(".with_operation(").count(),
        1,
        "only OverlayFs::with_generation may begin an operation; copy-up helpers must reuse it"
    );
    assert!(
        OVERLAY.contains("impl for<'operation> FnOnce(")
            && OVERLAY.contains("&LocationOperationView<'operation>"),
        "overlay must thread a higher-ranked operation view through each composite VFS callback"
    );
}

#[test]
fn dirfd_operations_reuse_one_higher_ranked_context_lease() {
    assert!(
        FILE_FS.contains(
            "impl for<'operation> FnOnce(FsContextOperationView<'operation>) -> AxResult<R>"
        ),
        "Starry's dirfd adapter must not expose a mutable FsContext that can start nested \
         admissions"
    );
    assert!(
        FILE_FS.contains("fs_context.lock().with_operation_scope(operation)"),
        "AT_FDCWD operations must enter one explicit filesystem operation scope"
    );
    assert!(
        FILE_FS.contains("directory.with_fs_context(&fs, operation)"),
        "directory-relative operations must reuse the opened directory's generation lease"
    );
    assert!(
        !FILE_FS.contains("impl FnOnce(&mut FsContext)"),
        "the old callback could silently admit nested operations after freeze started"
    );
}
