use std::{
    fs,
    path::{Path, PathBuf},
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-sync must remain below os/arceos/modules")
        .to_path_buf()
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("failed to inspect {}: {error}", path.display()))
        {
            let path = entry.expect("failed to read source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    sources
}

fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn assert_contains(path: &Path, expected: &str) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    assert!(
        compact(&source).contains(&compact(expected)),
        "{} must make its lock semantics explicit with `{expected}`",
        path.display(),
    );
}

fn assert_tree_contains(root: &Path, expected: &str) {
    let expected = compact(expected);
    let found = rust_sources(root).into_iter().any(|path| {
        fs::read_to_string(&path)
            .map(|source| compact(&source).contains(&expected))
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
    });
    assert!(
        found,
        "{} must make its lock semantics explicit with `{expected}`",
        root.display(),
    );
}

#[test]
fn selected_consumers_reject_the_ambiguous_ax_sync_mutex_alias() {
    let workspace = workspace_root();
    let consumers = [
        "os/arceos/modules/axinput/src",
        "os/arceos/modules/axdisplay/src",
        "os/arceos/modules/axfs-ng/src",
        "os/arceos/api/arceos_posix_api/src",
    ];
    let mut violations = Vec::new();

    for consumer in consumers {
        for path in rust_sources(&workspace.join(consumer)) {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            let compact = compact(&source);
            let direct_path =
                compact.contains("ax_sync::Mutex") || compact.contains("ax_sync::MutexGuard");
            let ambiguous_alias = compact.split(';').any(|statement| {
                statement.contains("ax_sync::")
                    && (statement.contains("asMutex") || statement.contains("asMutexGuard"))
            });
            let grouped_import = compact.split(';').any(|statement| {
                let Some((_, imports)) = statement.split_once("ax_sync::{") else {
                    return false;
                };
                imports
                    .split('}')
                    .next()
                    .expect("grouped import must have a first segment")
                    .split(',')
                    .map(|import| import.split("as").next().unwrap_or(import))
                    .any(|import| matches!(import, "Mutex" | "MutexGuard"))
            });
            if direct_path || ambiguous_alias || grouped_import {
                violations.push(path);
            }
        }
    }

    assert!(
        violations.is_empty(),
        "ambiguous ax_sync::Mutex imports hide spin-vs-sleep semantics: {violations:#?}"
    );
}

#[test]
fn consumer_lock_classes_match_their_waiting_behavior() {
    let workspace = workspace_root();

    for path in [
        "os/arceos/modules/axinput/src/lib.rs",
        "os/arceos/modules/axdisplay/src/lib.rs",
        "os/arceos/api/arceos_posix_api/src/imp/stdio.rs",
        "os/arceos/api/arceos_posix_api/src/imp/pipe.rs",
    ] {
        assert_contains(&workspace.join(path), "use ax_sync::SpinMutex;");
    }

    for path in [
        "os/arceos/api/arceos_posix_api/src/imp/fs.rs",
        "os/arceos/api/arceos_posix_api/src/imp/net.rs",
        "os/arceos/api/arceos_posix_api/src/imp/io_mpx/epoll.rs",
    ] {
        assert_contains(&workspace.join(path), "use ax_sync::PiMutex;");
    }

    assert_contains(
        &workspace.join("os/arceos/api/arceos_posix_api/src/imp/pthread/mutex.rs"),
        "use ax_sync::{PiMutex, SpinMutex};",
    );
    assert_contains(
        &workspace.join("os/arceos/modules/axfs-ng/src/os/sync.rs"),
        "pub use ax_sync::{PiMutex, PiMutexGuard, SpinMutex, SpinMutexGuard};",
    );
    let fat = workspace.join("os/arceos/modules/axfs-ng/src/fs/fat/fs.rs");
    assert_contains(&fat, "inner: PiMutex<FatFilesystemInner>");
    assert_contains(&fat, "root_dir: SpinMutex<Option<DirEntry>>");
    assert_tree_contains(
        &workspace.join("os/arceos/modules/axfs-ng/src/file/cache"),
        "static CACHED_FILE_BY_INODE: spin::LazyLock<SpinMutex<InodeCacheIndex>>",
    );
}

#[test]
fn sleepable_consumer_features_enable_pi_mutex_support() {
    let workspace = workspace_root();
    assert_contains(
        &workspace.join("os/arceos/modules/axfs-ng/Cargo.toml"),
        "ax-sync = { workspace = true, features = [\"multitask\"] }",
    );
    assert_contains(
        &workspace.join("os/arceos/modules/axfs-ng/Cargo.toml"),
        "lockdep = [\"axfs-ng-vfs/lockdep\", \"ax-sync/lockdep\"]",
    );
    let posix_manifest = workspace.join("os/arceos/api/arceos_posix_api/Cargo.toml");
    assert_contains(
        &posix_manifest,
        "fs = [\"multitask\", \"dep:ax-fs-ng\", \"fd\"]",
    );
    assert_contains(
        &posix_manifest,
        "net = [\"multitask\", \"dep:ax-net\", \"fd\"]",
    );
    assert_contains(&posix_manifest, "epoll = [\"multitask\", \"fd\"]");
}

#[test]
fn pi_mutex_context_failure_preserves_the_lock_callsite() {
    let workspace = workspace_root();
    let mutex = fs::read_to_string(workspace.join("os/arceos/modules/axsync/src/mutex.rs"))
        .expect("failed to read the PI mutex implementation");
    let mutex = compact(&mutex);

    assert!(
        mutex.contains("#[track_caller]fntask_result"),
        "PI task-runtime failures must retain the public lock callsite"
    );
    assert!(
        !mutex.contains("unwrap_or_else(|error|panic!"),
        "a panic inside an unwrap closure loses #[track_caller] propagation"
    );
}
