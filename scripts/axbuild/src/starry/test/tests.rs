mod board_tests;
mod host_http_tests;
mod qemu_discovery_tests;
mod qemu_run_tests;
mod summary_tests;
mod system_case_tests;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
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
        name: "qemu/system".to_string(),
        display_name: "qemu/system".to_string(),
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
        registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
        download_dir: workspace_root.join(".tgos-downloads"),
        extract_dir: workspace_root.join(".tgos-images"),
    };
    crate::image::config::ImageConfig::write_config(workspace_root, &config).unwrap();
}

#[cfg(unix)]
#[test]
fn aka_wifi_smoke_runs_one_connectivity_transfer() {
    let fake_bin = tempdir().unwrap();
    let invocation_log = fake_bin.path().join("iperf3-invocations");
    let ip = fake_bin.path().join("ip");
    let iperf3 = fake_bin.path().join("iperf3");

    fs::write(&ip, "#!/bin/sh\necho '2: wlan0    inet 192.0.2.2/24'\n").unwrap();
    fs::write(
        &iperf3,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >>\"$IPERF_INVOCATION_LOG\"\n",
    )
    .unwrap();
    for executable in [&ip, &iperf3] {
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-suit/starryos/board-aka-00-sg2002/wifi-iperf-smoke/iperf-smoke.sh");
    let output = Command::new("/bin/sh")
        .arg(script)
        .arg("192.0.2.1")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", fake_bin.path().display()),
        )
        .env("IPERF_INVOCATION_LOG", &invocation_log)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "smoke script failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(invocation_log).unwrap(),
        "-c 192.0.2.1 -t 3 -O 1 -P 1 -l 128K\n"
    );
}
