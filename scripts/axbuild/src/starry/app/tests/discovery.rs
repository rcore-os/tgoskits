use std::fs;

use tempfile::tempdir;

use super::discover_apps;
use crate::starry::app::{
    StarryAppKind,
    test_support::{write_case_file, write_minimal_board_case},
};

#[test]
fn discovers_prebuild_apps_and_ignores_listed_names() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "picoclaw-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "orangepi-5-plus-uvc",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "orangepi-5-plus-uvc-rknn",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    fs::write(
        root.path().join("apps/.ignore"),
        "apps/starry/orangepi-5-plus-uvc\napps/starry/orangepi-5-plus-uvc-rknn\n",
    )
    .unwrap();

    let apps = discover_apps(root.path()).unwrap();
    let names = apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>();

    assert!(names.contains(&"codex-cli"));
    assert!(names.contains(&"picoclaw-cli"));
    assert!(!names.contains(&"orangepi-5-plus-uvc"));
    assert!(!names.contains(&"orangepi-5-plus-uvc-rknn"));
}

#[test]
fn infers_qemu_and_board_app_kinds() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    write_minimal_board_case(root.path(), "board-demo");

    let apps = discover_apps(root.path()).unwrap();

    let board = apps.iter().find(|app| app.name == "board-demo").unwrap();
    let qemu = apps.iter().find(|app| app.name == "codex-cli").unwrap();
    assert_eq!(board.kind, StarryAppKind::Board);
    assert_eq!(qemu.kind, StarryAppKind::Qemu);
}
