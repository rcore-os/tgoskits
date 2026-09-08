mod board_tests;

mod host_http_tests;

#[cfg(unix)]
mod ltp_wrapper_tests;

mod nixos_tests;

mod qemu_discovery_tests;

mod qemu_run_tests;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
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
        grouped_command_selection: Default::default(),
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
            grouped_command_selection: Default::default(),
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
fn board_iperf2_smoke_requires_both_directions_and_propagates_failure() {
    let fake_bin = tempdir().unwrap();
    let invocation_log = fake_bin.path().join("iperf-invocations");
    let ip = fake_bin.path().join("ip");
    let iperf = fake_bin.path().join("iperf");
    let legacy_iperf = fake_bin.path().join("iperf3");
    fs::write(&legacy_iperf, "#!/bin/sh\nexit 99\n").unwrap();
    fs::write(&ip, "#!/bin/sh\necho '2: wlan0    inet 192.0.2.2/24'\n").unwrap();
    fs::write(
        &iperf,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >\"$IPERF_INVOCATION_LOG\"\nprintf '%s\\n' \
         \"$IPERF_OUTPUT\"\nexit \"$IPERF_STATUS\"\n",
    )
    .unwrap();
    for executable in [&ip, &iperf, &legacy_iperf] {
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-suit/starryos");
    std::os::unix::fs::symlink(
        root.join("board-common/iperf2/iperf2-smoke"),
        fake_bin.path().join("iperf2-smoke"),
    )
    .unwrap();
    for (case, duration, warmup, marker) in [
        (
            "board-aka-00-sg2002/wifi-iperf-smoke",
            22,
            2,
            "STARRY_AKA_WIFI_IPERF_SMOKE",
        ),
        (
            "board-orangepi-5-plus/native-network-smoke",
            4,
            1,
            "STARRY_IPERF_SMOKE",
        ),
    ] {
        let report = |stalled_direction: Option<&str>, omit_receive: bool, short: bool| {
            let mut output = String::new();
            for direction in [" 1", "*1"] {
                if omit_receive && direction == "*1" {
                    continue;
                }
                for second in 0..duration {
                    let bytes = if second < warmup
                        || (stalled_direction == Some(direction)
                            && second >= warmup
                            && second < warmup + 3)
                    {
                        0
                    } else {
                        524288
                    };
                    output.push_str(&format!(
                        "[ {direction}] {second}.0000-{}.0000 sec {bytes} Bytes 4194304 bits/sec\n",
                        second + 1
                    ));
                }
                let end = if short { 1 } else { duration };
                output.push_str(&format!(
                    "[ {direction}] 0.0000-{end}.0200 sec 8388608 Bytes 3355443 bits/sec\n"
                ));
            }
            output
        };
        for (name, report, status, expected) in [
            (
                "bidirectional progress",
                report(None, false, false),
                "0",
                true,
            ),
            ("TX stall", report(Some(" 1"), false, false), "0", false),
            ("RX stall", report(Some("*1"), false, false), "0", false),
            ("missing RX", report(None, true, false), "0", false),
            ("short transfer", report(None, false, true), "0", false),
            ("empty report", String::new(), "0", false),
            ("iperf error", report(None, false, false), "1", false),
        ] {
            let output = Command::new("/bin/sh")
                .arg(root.join(case).join("iperf-smoke.sh"))
                .arg("192.0.2.1")
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", fake_bin.path().display()),
                )
                .env("IPERF_INVOCATION_LOG", &invocation_log)
                .env("IPERF_OUTPUT", report)
                .env("IPERF_STATUS", status)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert_eq!(
                output.status.success(),
                expected,
                "{case}: {name}: {stdout}"
            );
            assert_eq!(stdout.contains(&format!("{marker}_PASSED")), expected);
            assert_eq!(stdout.contains(&format!("{marker}_FAILED")), !expected);
            assert_eq!(
                fs::read_to_string(&invocation_log).unwrap(),
                format!(
                    "-c 192.0.2.1 -p 5001 -t {duration} -i 1 -P 1 -l 128K -f b --full-duplex\n"
                )
            );
        }
    }
}
