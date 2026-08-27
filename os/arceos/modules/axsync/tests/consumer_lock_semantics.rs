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

fn assert_not_contains(path: &Path, forbidden: &str) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    assert!(
        !compact(&source).contains(&compact(forbidden)),
        "{} must not retain `{forbidden}`",
        path.display(),
    );
}

#[test]
fn os_consumers_do_not_bypass_the_runtime_lock_facade() {
    let workspace = workspace_root();
    let consumers = [
        "os/StarryOS/kernel/src",
        "os/arceos/ulib/axstd/src",
        "os/axvisor/src",
        "os/arceos/api/arceos_posix_api/src",
    ];
    let mut violations = Vec::new();

    for consumer in consumers {
        for path in rust_sources(&workspace.join(consumer)) {
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            if source.contains("ax_sync::") || source.contains("use ax_sync") {
                violations.push(path);
            }
        }
    }

    assert!(
        violations.is_empty(),
        "OS consumers must import locks from their runtime facade: {violations:#?}"
    );
}

#[test]
fn consumers_use_their_layer_owned_lock_interfaces() {
    let workspace = workspace_root();

    for path in [
        "os/arceos/modules/axinput/src/lib.rs",
        "os/arceos/modules/axdisplay/src/lib.rs",
    ] {
        assert_contains(
            &workspace.join(path),
            "use ax_task::sync::SpinLock as Mutex;",
        );
    }

    let posix_facade = workspace.join("os/arceos/api/arceos_posix_api/src/sync.rs");
    assert_contains(&posix_facade, "pub(crate) use ax_runtime::sync::Mutex;");
    assert_not_contains(&posix_facade, "SpinLock as Mutex");
    let fs_facade = workspace.join("fs/ax-fs-ng/src/os/sync.rs");
    assert_contains(
        &fs_facade,
        "pub use ax_sync::{Mutex, MutexGuard, SpinLock, SpinLockGuard};",
    );
    assert_not_contains(&fs_facade, "PiMutex");
    assert_not_contains(&fs_facade, "SpinMutex");
}

#[test]
fn sleepable_external_consumers_enable_only_the_sleep_abi() {
    let workspace = workspace_root();
    assert_contains(
        &workspace.join("fs/ax-fs-ng/Cargo.toml"),
        "ax-sync = { workspace = true, features = [\"sleep\"] }",
    );
    assert_contains(
        &workspace.join("fs/ax-fs-ng/Cargo.toml"),
        "lockdep = [\"axfs-ng-vfs/lockdep\"]",
    );
    let posix_manifest = workspace.join("os/arceos/api/arceos_posix_api/Cargo.toml");
    assert_contains(&posix_manifest, "fs = [\"dep:ax-fs-ng\", \"fd\"]");
    assert_contains(&posix_manifest, "net = [\"dep:ax-net\", \"fd\"]");
    assert_contains(&posix_manifest, "epoll = [\"fd\"]");
    assert_not_contains(&posix_manifest, "multitask");
}
