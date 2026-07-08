use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::tempdir;

use super::{app_qemu_test_case, load_qemu_app_case_fields, resolve_qemu_config};
use crate::{
    starry::app::{
        StarryAppQemuCase, discover_apps,
        test_support::{write_case_file, write_test_image_config},
    },
    test::case::HostHttpServerConfig,
};

#[test]
fn qemu_config_selection_prefers_exact_arch_config() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    let exact = write_case_file(root.path(), "codex-cli", "qemu-x86_64.toml", "args = []\n");
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let selected = resolve_qemu_config(&app, Some("x86_64"), None)
        .unwrap()
        .unwrap();

    assert_eq!(selected, exact);
}

#[test]
fn qemu_config_selection_rejects_variant_only_default() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let err = resolve_qemu_config(&app, Some("x86_64"), None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("qemu-x86_64.toml"));
}

#[test]
fn qemu_config_selection_uses_explicit_variant_config() {
    let root = tempdir().unwrap();
    let explicit = write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-syscall-hunt.toml",
        "args = []\n",
    );
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let selected = resolve_qemu_config(
        &app,
        Some("x86_64"),
        Some(Path::new("qemu-x86_64-codex-syscall-hunt.toml")),
    )
    .unwrap()
    .unwrap();

    assert_eq!(selected, explicit);
}

#[test]
fn qemu_case_fields_load_grouped_commands_and_subcases() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "qemu/sqlite",
        "qemu-x86_64.toml",
        "args = []\nuefi = false\nto_bin = true\nsuccess_regex = []\nfail_regex = \
         []\ntest_commands = [\"/usr/bin/app-sqlite\", \"/usr/bin/app-sqlite-deep\"]\n",
    );
    write_case_file(
        root.path(),
        "qemu/sqlite/app-sqlite/c",
        "CMakeLists.txt",
        "cmake_minimum_required(VERSION 3.20)\n",
    );
    write_case_file(
        root.path(),
        "qemu/sqlite/app-sqlite-deep/c",
        "CMakeLists.txt",
        "cmake_minimum_required(VERSION 3.20)\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "qemu/sqlite")
        .unwrap();
    let qemu_config = resolve_qemu_config(&app, Some("x86_64"), None).unwrap();

    let fields =
        load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

    assert_eq!(
        fields.test_case.test_commands,
        vec!["/usr/bin/app-sqlite", "/usr/bin/app-sqlite-deep"]
    );
    assert_eq!(fields.test_case.subcases.len(), 2);
}

#[test]
fn qemu_case_fields_load_configured_managed_rootfs() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let rootfs_path = root
        .path()
        .join(".tgos-images/rootfs-aarch64-debian.img/rootfs-aarch64-debian.img");
    write_case_file(
        root.path(),
        "qemu/apt",
        "qemu-aarch64.toml",
        r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-aarch64-debian.img/rootfs-aarch64-debian.img",
]
uefi = false
to_bin = true
success_regex = []
fail_regex = []
"#,
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "qemu/apt")
        .unwrap();
    let qemu_config = resolve_qemu_config(&app, Some("aarch64"), None).unwrap();

    let fields =
        load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

    assert_eq!(fields.rootfs_path, Some(rootfs_path));
    assert!(fields.snapshot);
}

#[test]
fn qemu_case_fields_load_snapshot_disable() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "macos-selfbuild",
        "qemu-aarch64.toml",
        r#"args = []
uefi = false
to_bin = true
snapshot = false
success_regex = []
fail_regex = []
"#,
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "macos-selfbuild")
        .unwrap();
    let qemu_config = resolve_qemu_config(&app, Some("aarch64"), None).unwrap();

    let fields =
        load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

    assert!(!fields.snapshot);
}

#[test]
fn app_qemu_test_case_preserves_host_symbolize_success_regex() {
    let case_dir = PathBuf::from("/tmp/apps/starry/memtrack-backtrace");
    let qemu_config_path = case_dir.join("qemu-x86_64.toml");
    let case = StarryAppQemuCase {
        name: "memtrack-backtrace".to_string(),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        build_config_path: None,
        qemu_config_path: Some(qemu_config_path.clone()),
        rootfs_path: PathBuf::from("/tmp/rootfs.img"),
        snapshot: true,
        test_commands: Vec::new(),
        host_symbolize_success_regex: vec!["symbolized".to_string()],
        host_http_server: Some(HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 18382,
            body: "fixture".to_string(),
            body_size: None,
            body_byte: b'X',
            dir: None,
        }),
        subcases: Vec::new(),
    };

    let test_case = app_qemu_test_case(&case, case_dir.clone()).unwrap();

    assert_eq!(test_case.case_dir, case_dir);
    assert_eq!(test_case.qemu_config_path, qemu_config_path);
    assert_eq!(test_case.host_symbolize_success_regex, vec!["symbolized"]);
    assert_eq!(
        test_case
            .host_http_server
            .as_ref()
            .map(|config| (config.bind.as_str(), config.port)),
        Some(("127.0.0.1", 18382))
    );
}

#[test]
fn claw_code_prebuild_replaces_stale_rootfs_directory() {
    let root = tempdir().unwrap();
    let workspace = root.path();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("axbuild manifest should live under scripts/axbuild")
        .to_path_buf();
    let script = repo.join("apps/starry/claw-code/prebuild.sh");

    let cache = workspace.join("cache");
    let bin = cache.join("claw");
    fs::create_dir_all(&cache).unwrap();
    fs::write(&bin, b"fake claw").unwrap();

    let tools = workspace.join("tools");
    fs::create_dir_all(&tools).unwrap();
    let debugfs = tools.join("debugfs");
    fs::write(
        &debugfs,
        "#!/usr/bin/env bash\nif [ \"$1\" = \"-w\" ]; then test -f \"$2\"; fi\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&debugfs, fs::Permissions::from_mode(0o755)).unwrap();

    let rootfs_dir = workspace.join("tmp/axbuild/rootfs");
    let default_rootfs = rootfs_dir.join("rootfs-x86_64-alpine.img");
    let app_rootfs = rootfs_dir.join("rootfs-x86_64-claw-code.img");
    fs::create_dir_all(&default_rootfs).unwrap();
    fs::write(
        default_rootfs.join("rootfs-x86_64-alpine.img"),
        b"base rootfs",
    )
    .unwrap();
    fs::create_dir_all(&app_rootfs).unwrap();

    let path = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap());
    let status = Command::new("bash")
        .arg(&script)
        .current_dir(repo.join("apps/starry/claw-code"))
        .env("CLAW_CACHE_DIR", &cache)
        .env("STARRY_WORKSPACE", workspace)
        .env("STARRY_ROOTFS", &app_rootfs)
        .env("STARRY_OVERLAY_DIR", workspace.join("overlay"))
        .env("PATH", path)
        .status()
        .unwrap();

    assert!(status.success());
    assert!(app_rootfs.is_file());
    assert_eq!(fs::read(&app_rootfs).unwrap(), b"base rootfs");
    assert_eq!(
        fs::read(default_rootfs.join("rootfs-x86_64-alpine.img")).unwrap(),
        b"base rootfs"
    );
}
