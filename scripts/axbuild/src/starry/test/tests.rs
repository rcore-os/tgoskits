mod asset_equivalence_tests;
mod asset_network_tests;
mod asset_package_tests;
mod board_tests;
mod host_http_tests;
mod qemu_discovery_tests;
mod qemu_run_tests;
mod summary_tests;
mod system_case_tests;

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use ostool::run::qemu::QemuConfig;
use tempfile::tempdir;

use super::*;
use crate::{
    context::ResolvedStarryRequest,
    test::{
        case,
        case::{TestQemuCase, TestQemuSubcaseKind},
        qemu as qemu_test,
    },
};

fn write_qemu_build_config(root: &Path, _group: &str, build_group: &str, target: &str) -> PathBuf {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(format!("build-{target}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!("target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \"Info\"\n"),
    )
    .unwrap();
    path
}

fn write_flat_qemu_build_config(root: &Path, build_group: &str, target: &str) -> PathBuf {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(format!("build-{target}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!("target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \"Info\"\n"),
    )
    .unwrap();
    path
}

fn write_qemu_build_config_with_max_cpu_num(
    root: &Path,
    _group: &str,
    build_group: &str,
    target: &str,
    max_cpu_num: usize,
) -> PathBuf {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(format!("build-{target}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \
             \"Info\"\nmax_cpu_num = {max_cpu_num}\n"
        ),
    )
    .unwrap();
    path
}

fn write_starry_board_build_config(root: &Path, build_group: &str, target: &str) -> PathBuf {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(format!("build-{target}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!("target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \"Info\"\n"),
    )
    .unwrap();
    path
}

fn starry_request(path: PathBuf, arch: &str, target: &str) -> ResolvedStarryRequest {
    ResolvedStarryRequest {
        package: crate::context::STARRY_PACKAGE.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path: path,
        build_info_override: None,
        qemu_config: None,
        uboot_config: None,
    }
}

fn write_board_test_config(
    root: &Path,
    build_group: &str,
    case_name: &str,
    board_name: &str,
) -> PathBuf {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(case_name)
        .join(format!("board-{board_name}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
         \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
         pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = 300\n",
    )
    .unwrap();
    path
}

fn write_qemu_test_config(
    root: &Path,
    _group: &str,
    build_group: &str,
    case_name: &str,
    arch: &str,
) {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(case_name)
        .join(format!("qemu-{arch}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "timeout = 1\n").unwrap();
}

fn write_grouped_qemu_test_config(
    root: &Path,
    _group: &str,
    build_group: &str,
    case_name: &str,
    arch: &str,
) {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(case_name)
        .join(format!("qemu-{arch}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        "shell_prefix = \"root@starry:\"\ntest_commands = [\"/usr/bin/beta\", \
         \"/usr/bin/alpha\"]\ntimeout = 1\n",
    )
    .unwrap();
}

fn write_flat_grouped_qemu_test_config(
    root: &Path,
    build_group: &str,
    case_name: &str,
    arch: &str,
) {
    let path = root
        .join("test-suit/starryos")
        .join(build_group)
        .join(case_name)
        .join(format!("qemu-{arch}.toml"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        "shell_prefix = \"root@starry:\"\ntest_commands = [\"/usr/bin/starry-run-all\"]\ntimeout \
         = 1\n",
    )
    .unwrap();
}

fn grouped_host_http_test_case(
    case_dir: &Path,
    grouped_subcase_filter: Option<BTreeSet<String>>,
) -> crate::test::case::TestQemuCase {
    crate::test::case::TestQemuCase {
        name: "qemu-smp1/system".to_string(),
        display_name: "qemu-smp1/system".to_string(),
        case_dir: case_dir.to_path_buf(),
        qemu_config_path: case_dir.join("qemu-x86_64.toml"),
        test_commands: Vec::new(),
        host_symbolize_success_regex: Vec::new(),
        host_http_server: Some(crate::test::case::HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 18380,
            body: "fixture".to_string(),
            body_size: Some(4),
            body_byte: b'Z',
            dir: None,
        }),
        subcases: grouped_subcase_filter
            .as_ref()
            .into_iter()
            .flatten()
            .map(|name| crate::test::case::TestQemuSubcase {
                name: name.clone(),
                case_dir: case_dir.join(name),
                kind: crate::test::case::TestQemuSubcaseKind::C,
            })
            .collect(),
        grouped_subcase_filter,
    }
}

fn prepared_qemu_case(name: &str, build_config_path: PathBuf) -> PreparedStarryQemuCase {
    PreparedStarryQemuCase {
        case: crate::test::case::TestQemuCase {
            name: name.to_string(),
            display_name: name.to_string(),
            case_dir: PathBuf::from(format!("/tmp/{name}")),
            qemu_config_path: PathBuf::from(format!("/tmp/{name}/qemu-x86_64.toml")),
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: None,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        },
        qemu: QemuConfig::default(),
        build_group: "default".to_string(),
        build_config_path,
        rootfs_path: PathBuf::from("/tmp/rootfs.img"),
        requirements: StarryQemuCaseRequirements { smp: 1 },
    }
}

fn write_test_image_config(workspace_root: &Path) {
    let config = crate::image::config::ImageConfig {
        local_storage: workspace_root.join(".tgos-images"),
        registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
        auto_sync: true,
        auto_sync_threshold: 60,
    };
    crate::image::config::ImageConfig::write_config(workspace_root, &config).unwrap();
}
