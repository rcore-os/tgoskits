use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const ORANGEPI_PROFILE: &str = include_str!("../../board-orangepi-5-plus.toml");
const LICHEERV_NANO_PROFILE: &str = include_str!("../../board-licheerv-nano-sg2002.toml");
const AKA_00_PROFILE: &str = include_str!("../../board-aka-00-sg2002.toml");
const VISIONFIVE2_PROFILE: &str = include_str!("../../board-visionfive2.toml");
const PHYTIUMPI_PROFILE: &str = include_str!("../../board-phytiumpi.toml");
const RK3568_PROFILE: &str = include_str!("../../board-roc-rk3568-pc.toml");
const ROCK4D_PROFILE: &str = include_str!("../../board-rock-4d.toml");
const JL_LSGD2K10_PROFILE: &str = include_str!("../../board-jl-lsgd2k10.toml");
const INIT_SCRIPT: &str = include_str!("../../init.sh");

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("block-rw-bench-init-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_init_script(prelude: &str, env: &[(&str, &Path)]) -> std::process::Output {
    let mut child = Command::new("dash")
        .arg("-s")
        .envs(env.iter().map(|(key, value)| (*key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let script = format!("{prelude}\n{INIT_SCRIPT}");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn linux_staged_helper_runs_without_probing_starry_network() {
    let dir = TestDir::new();
    let staged = dir.path().join("staged-helper");
    let program = dir.path().join("program");
    let workdir = dir.path().join("work");
    let ip_probe = dir.path().join("ip-probe");
    let copy_probe = dir.path().join("copy-probe");
    fs::write(
        &staged,
        "#!/bin/sh\nprintf '%s\\n' \"$BLOCK_RW_BENCH_SUCCESS_MARKER\"\n",
    )
    .unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run_init_script(
        r#"
ip() {
  printf invoked > "$BLOCK_RW_BENCH_IP_PROBE"
  return 1
}
cp() {
  printf invoked > "$BLOCK_RW_BENCH_COPY_PROBE"
  return 1
}
"#,
        &[
            ("BLOCK_RW_BENCH_STAGED_PROGRAM", &staged),
            ("BLOCK_RW_BENCH_PROGRAM", &program),
            ("BLOCK_RW_BENCH_WORKDIR", &workdir),
            ("BLOCK_RW_BENCH_IP_PROBE", &ip_probe),
            ("BLOCK_RW_BENCH_COPY_PROBE", &copy_probe),
            ("BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS", Path::new("1")),
            (
                "BLOCK_RW_BENCH_SUCCESS_MARKER",
                Path::new("TEST_BLOCK_RW_BENCH_PASSED"),
            ),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "TEST_BLOCK_RW_BENCH_PASSED")
    );
    assert!(
        !ip_probe.exists(),
        "the Starry path must not preflight with `ip`"
    );
    assert!(
        !copy_probe.exists(),
        "a staged helper must execute in place instead of being copied"
    );
}

#[test]
fn session_http_retries_are_bounded_and_do_not_probe_with_ip() {
    let dir = TestDir::new();
    let staged = dir.path().join("missing-staged-helper");
    let helper = dir.path().join("session-helper");
    let program = dir.path().join("program");
    let workdir = dir.path().join("work");
    let counter = dir.path().join("curl-count");
    let ip_probe = dir.path().join("ip-probe");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s\\n' \"$BLOCK_RW_BENCH_SUCCESS_MARKER\"\n",
    )
    .unwrap();

    let output = run_init_script(
        r#"
ip() {
  printf invoked > "$BLOCK_RW_BENCH_IP_PROBE"
  return 1
}
curl() {
  count=0
  if [ -f "$BLOCK_RW_BENCH_CURL_COUNT" ]; then
    count="$(cat "$BLOCK_RW_BENCH_CURL_COUNT")"
  fi
  count=$(( count + 1 ))
  printf '%s' "$count" > "$BLOCK_RW_BENCH_CURL_COUNT"
  if [ "$count" -lt 3 ]; then
    return 1
  fi
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
      shift
      cp "$BLOCK_RW_BENCH_FAKE_HELPER" "$1"
      return
    fi
    shift
  done
  return 1
}
"#,
        &[
            ("BLOCK_RW_BENCH_STAGED_PROGRAM", &staged),
            ("BLOCK_RW_BENCH_PROGRAM", &program),
            ("BLOCK_RW_BENCH_WORKDIR", &workdir),
            ("BLOCK_RW_BENCH_CURL_COUNT", &counter),
            ("BLOCK_RW_BENCH_FAKE_HELPER", &helper),
            ("BLOCK_RW_BENCH_IP_PROBE", &ip_probe),
            ("BLOCK_RW_BENCH_DOWNLOAD_ATTEMPTS", Path::new("3")),
            ("BLOCK_RW_BENCH_DOWNLOAD_RETRY_SECONDS", Path::new("0")),
            (
                "BLOCK_RW_BENCH_SUCCESS_MARKER",
                Path::new("TEST_BLOCK_RW_BENCH_PASSED"),
            ),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "TEST_BLOCK_RW_BENCH_PASSED")
    );
    assert_eq!(fs::read_to_string(counter).unwrap(), "3");
    assert!(!ip_probe.exists(), "download retries must not invoke `ip`");
}

#[test]
fn exhausted_session_http_retries_emit_only_the_failure_marker() {
    let dir = TestDir::new();
    let staged = dir.path().join("missing-staged-helper");
    let program = dir.path().join("program");
    let workdir = dir.path().join("work");
    let counter = dir.path().join("curl-count");

    let output = run_init_script(
        r#"
curl() {
  count=0
  if [ -f "$BLOCK_RW_BENCH_CURL_COUNT" ]; then
    count="$(cat "$BLOCK_RW_BENCH_CURL_COUNT")"
  fi
  count=$(( count + 1 ))
  printf '%s' "$count" > "$BLOCK_RW_BENCH_CURL_COUNT"
  if [ "$count" -eq 2 ]; then
    printf 'root@starry:/root # '
  fi
  return 1
}
"#,
        &[
            ("BLOCK_RW_BENCH_STAGED_PROGRAM", &staged),
            ("BLOCK_RW_BENCH_PROGRAM", &program),
            ("BLOCK_RW_BENCH_WORKDIR", &workdir),
            ("BLOCK_RW_BENCH_CURL_COUNT", &counter),
            ("BLOCK_RW_BENCH_DOWNLOAD_ATTEMPTS", Path::new("2")),
            ("BLOCK_RW_BENCH_DOWNLOAD_RETRY_SECONDS", Path::new("0")),
            (
                "BLOCK_RW_BENCH_SUCCESS_MARKER",
                Path::new("TEST_BLOCK_RW_BENCH_PASSED"),
            ),
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "TEST_BLOCK_RW_BENCH_SESSION_FAILED")
    );
    assert!(
        !stdout
            .lines()
            .any(|line| line == "TEST_BLOCK_RW_BENCH_PASSED"),
        "a missing helper must never produce the workload success marker"
    );
    assert_eq!(fs::read_to_string(counter).unwrap(), "2");
}

#[test]
fn board_profiles_require_the_uploaded_session_helper() {
    for (board, profile) in [
        ("OrangePi", ORANGEPI_PROFILE),
        ("LicheeRV Nano", LICHEERV_NANO_PROFILE),
        ("AKA-00", AKA_00_PROFILE),
        ("VisionFive2", VISIONFIVE2_PROFILE),
        ("PhytiumPi", PHYTIUMPI_PROFILE),
        ("ROC-RK3568-PC", RK3568_PROFILE),
        ("Rock-4D", ROCK4D_PROFILE),
        ("JL-LSGD2K10", JL_LSGD2K10_PROFILE),
    ] {
        assert!(
            !profile.contains("BLOCK_RW_BENCH_INLINE_FALLBACK"),
            "{board} must not turn a missing session helper into another successful workload"
        );
        assert!(
            !profile.contains("BLOCK_RW_BENCH_NETWORK_WAIT_SECONDS"),
            "{board} must not use an `ip`-based network preflight"
        );
        assert!(
            profile.contains("_SESSION_FAILED"),
            "{board} must classify an unavailable session helper as failure"
        );
    }

    assert!(
        !INIT_SCRIPT.contains("run_inline_fallback"),
        "the board test must have one success-producing workload"
    );
    assert!(
        !INIT_SCRIPT.contains("ip -4"),
        "the board test must retry the actual transfer instead of guessing network readiness"
    );
    assert!(INIT_SCRIPT.contains("BLOCK_RW_BENCH_STAGED_PROGRAM"));
    assert!(INIT_SCRIPT.contains("BLOCK_RW_BENCH_DOWNLOAD_ATTEMPTS"));
}

#[test]
fn rock4d_profile_describes_the_rk3576_dwcmshc_emmc_path() {
    assert!(ROCK4D_PROFILE.contains("board_type = \"Rock-4D\""));
    assert!(ROCK4D_PROFILE.contains("export BLOCK_RW_BENCH_ROOT_DEVICE='/dev/mmcblk0'"));
    assert!(ROCK4D_PROFILE.contains("export BLOCK_RW_BENCH_CONTROLLER='rk3588-dwcmshc-emmc'"));
    assert!(ROCK4D_PROFILE.contains("export BLOCK_RW_BENCH_MAX_TRANSFER_BYTES='1048064'"));
    assert!(ROCK4D_PROFILE.contains("ROCK4D_BLOCK_RW_BENCH_PASSED"));
}

#[test]
fn jl_profile_uses_the_linux_autologin_staging_path() {
    assert!(
        JL_LSGD2K10_PROFILE
            .contains("export BLOCK_RW_BENCH_STAGED_PROGRAM='/home/loongson/block-rw-bench'"),
        "JL Linux staging must use a path writable by its non-root automatic-login user"
    );
}
