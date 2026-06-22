use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn write_case_file(root: &Path, case_name: &str, name: &str, body: &str) -> PathBuf {
    let path = root.join("apps/starry").join(case_name).join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

pub(super) fn write_board_default(root: &Path, board_name: &str, target: &str) -> PathBuf {
    let path = root
        .join("os/StarryOS/configs/board")
        .join(format!("{board_name}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "target = \"{target}\"\nenv = {{}}\nfeatures = []\nlog = \"Info\"\nplat_dyn = true\n"
        ),
    )
    .unwrap();
    path
}

pub(super) fn write_minimal_board_case(root: &Path, case_name: &str) {
    write_case_file(root, case_name, "init.sh", "echo hello\n");
    write_case_file(
        root,
        case_name,
        "board-orangepi-5-plus.toml",
        "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
    );
    write_case_file(
        root,
        case_name,
        "build-aarch64-unknown-none-softfloat.toml",
        "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = []\nlog = \"Info\"\n",
    );
}

pub(super) fn write_test_image_config(workspace_root: &Path) {
    let config = crate::image::config::ImageConfig {
        local_storage: workspace_root.join(".tgos-images"),
        registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
        auto_sync: true,
        auto_sync_threshold: 60,
    };
    crate::image::config::ImageConfig::write_config(workspace_root, &config).unwrap();
}
