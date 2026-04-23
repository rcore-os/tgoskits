//! StarryOS test case asset orchestration.
//!
//! Main responsibilities:
//! - Decide whether a test case needs extra build or injection work
//! - Prepare case-scoped work directories, overlays, and auxiliary QEMU assets
//! - Dispatch C and shell case flows before rootfs content injection

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use ostool::run::qemu::QemuConfig;

use super::{case_build, test_suit::StarryQemuCase};

pub(crate) const GROUPED_CASE_RUNNER_PATH: &str = "/usr/bin/starry-run-case-tests";
pub(crate) const GROUPED_CASE_SUCCESS_REGEX: &str = r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$";
pub(crate) const GROUPED_CASE_FAIL_REGEX: &str = r"(?m)^STARRY_GROUPED_TEST_FAILED:";

const CASE_WORK_ROOT_NAME: &str = "starry-cases";
const CASE_STAGING_DIR_NAME: &str = "staging-root";
const CASE_BUILD_DIR_NAME: &str = "build";
const CASE_OVERLAY_DIR_NAME: &str = "overlay";
const CASE_COMMAND_WRAPPER_DIR_NAME: &str = "guest-bin";
const CASE_CROSS_BIN_DIR_NAME: &str = "cross-bin";
const CASE_CMAKE_TOOLCHAIN_FILE_NAME: &str = "cmake-toolchain.cmake";
const CASE_APK_CACHE_DIR_NAME: &str = "apk-cache";
const CASE_SH_DIR_NAME: &str = "sh";
const GROUPED_CASE_RUNNER_NAME: &str = "starry-run-case-tests";
const USB_STICK_IMAGE_NAME: &str = "usb-stick.raw";
const USB_STICK_IMAGE_SIZE: u64 = 10 * 1024 * 1024;
const USB_AUDIO_OUTPUT_PCAP_NAME: &str = "usb-audio-iso.pcap";
const USB_AUDIO_REFERENCE_WAV_REL_PATH: &str = "c/assets/reference.wav";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryCaseAssets {
    pub(crate) rootfs_path: PathBuf,
    pub(crate) extra_qemu_args: Vec<String>,
    host_post_check: Option<CaseHostPostCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaseAssetLayout {
    pub(crate) work_dir: PathBuf,
    pub(crate) staging_root: PathBuf,
    pub(crate) build_dir: PathBuf,
    pub(crate) overlay_dir: PathBuf,
    pub(crate) command_wrapper_dir: PathBuf,
    pub(crate) cross_bin_dir: PathBuf,
    pub(crate) cmake_toolchain_file: PathBuf,
    pub(crate) apk_cache_dir: PathBuf,
    pub(crate) usb_stick_path: PathBuf,
    pub(crate) usb_audio_output_pcap_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CaseHostPostCheck {
    UsbAudioIso { output_pcap_path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsbQemuProfile {
    Storage,
    AudioIso,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPcmWave {
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    data: Vec<u8>,
}

/// Resolves the workspace target directory used for a Starry build target.
pub(crate) fn resolve_target_dir(workspace_root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let _ = crate::context::starry_arch_for_target_checked(target)?;
    Ok(workspace_root.join("target").join(target))
}

/// Prepares any case-specific rootfs assets required by a Starry QEMU test.
pub(crate) async fn prepare_case_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryQemuCase,
    rootfs_path: PathBuf,
) -> anyhow::Result<StarryCaseAssets> {
    let needs_assets = case.is_grouped()
        || case_uses_c_pipeline(case)
        || case_uses_sh_pipeline(case)
        || case_uses_python_pipeline(case)
        || case_uses_usb_qemu_assets(arch, case);

    if !needs_assets {
        return Ok(StarryCaseAssets {
            rootfs_path,
            extra_qemu_args: Vec::new(),
            host_post_check: None,
        });
    }

    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let rootfs_path_for_task = rootfs_path.clone();
    let case = case.clone();
    let (extra_qemu_args, host_post_check) = tokio::task::spawn_blocking(move || {
        prepare_case_assets_sync(
            &workspace_root,
            &arch,
            &target,
            &case,
            &rootfs_path_for_task,
        )
    })
    .await
    .context("starry case asset task failed")??;

    Ok(StarryCaseAssets {
        rootfs_path,
        extra_qemu_args,
        host_post_check,
    })
}

/// Returns whether a Starry test case uses the C pipeline.
pub(crate) fn case_uses_c_pipeline(case: &StarryQemuCase) -> bool {
    case_build::case_c_source_dir(case).is_dir()
}

/// Returns the shell-script source directory for a Starry test case.
pub(crate) fn case_sh_source_dir(case: &StarryQemuCase) -> PathBuf {
    case.case_dir.join(CASE_SH_DIR_NAME)
}

/// Returns whether a Starry test case uses the shell pipeline.
pub(crate) fn case_uses_sh_pipeline(case: &StarryQemuCase) -> bool {
    case_sh_source_dir(case).is_dir()
}

/// Returns the Python source directory for a Starry test case.
pub(crate) fn case_python_source_dir(case: &StarryQemuCase) -> PathBuf {
    case_build::case_python_source_dir(case)
}

/// Returns whether a Starry test case uses the Python pipeline.
pub(crate) fn case_uses_python_pipeline(case: &StarryQemuCase) -> bool {
    case_python_source_dir(case).is_dir()
}

/// Returns whether a Starry test case needs extra USB-backed QEMU assets.
pub(crate) fn case_uses_usb_qemu_assets(arch: &str, case: &StarryQemuCase) -> bool {
    case_usb_qemu_profile(arch, case).is_some()
}

fn case_usb_qemu_profile(arch: &str, case: &StarryQemuCase) -> Option<UsbQemuProfile> {
    let _ = arch;
    match case.name.as_str() {
        "usb" => Some(UsbQemuProfile::Storage),
        "usb-audio-iso" => Some(UsbQemuProfile::AudioIso),
        _ => None,
    }
}

/// Builds the working directory layout used for a Starry case asset run.
pub(crate) fn case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(CASE_WORK_ROOT_NAME).join(case_name);

    Ok(CaseAssetLayout {
        staging_root: work_dir.join(CASE_STAGING_DIR_NAME),
        build_dir: work_dir.join(CASE_BUILD_DIR_NAME),
        overlay_dir: work_dir.join(CASE_OVERLAY_DIR_NAME),
        command_wrapper_dir: work_dir.join(CASE_COMMAND_WRAPPER_DIR_NAME),
        cross_bin_dir: work_dir.join(CASE_CROSS_BIN_DIR_NAME),
        cmake_toolchain_file: work_dir.join(CASE_CMAKE_TOOLCHAIN_FILE_NAME),
        apk_cache_dir: work_dir.join(CASE_APK_CACHE_DIR_NAME),
        usb_stick_path: work_dir.join(USB_STICK_IMAGE_NAME),
        usb_audio_output_pcap_path: work_dir.join(USB_AUDIO_OUTPUT_PCAP_NAME),
        work_dir,
    })
}

/// Performs the synchronous part of Starry case asset preparation.
fn prepare_case_assets_sync(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryQemuCase,
    case_rootfs: &Path,
) -> anyhow::Result<(Vec<String>, Option<CaseHostPostCheck>)> {
    let layout = case_asset_layout(workspace_root, target, &case.name)?;
    fs::create_dir_all(&layout.work_dir)
        .with_context(|| format!("failed to create {}", layout.work_dir.display()))?;

    if case.is_grouped() {
        case_build::prepare_grouped_case_assets_sync(arch, case, case_rootfs, &layout)?;
    } else if case_uses_c_pipeline(case) {
        case_build::prepare_c_case_assets_sync(arch, case, case_rootfs, &layout)?;
    } else if case_uses_sh_pipeline(case) {
        prepare_sh_case_assets_sync(case, case_rootfs, &layout)?;
    } else if case_uses_python_pipeline(case) {
        case_build::prepare_python_case_assets_sync(arch, case, case_rootfs, &layout)?;
    }

    let (extra_qemu_args, host_post_check) = match case_usb_qemu_profile(arch, case) {
        Some(UsbQemuProfile::Storage) => {
            create_usb_backing_image(&layout.usb_stick_path)?;
            (usb_qemu_args(&layout.usb_stick_path), None)
        }
        Some(UsbQemuProfile::AudioIso) => {
            if layout.usb_audio_output_pcap_path.exists() {
                fs::remove_file(&layout.usb_audio_output_pcap_path).with_context(|| {
                    format!(
                        "failed to remove stale {}",
                        layout.usb_audio_output_pcap_path.display()
                    )
                })?;
            }
            (
                usb_audio_qemu_args(&layout.usb_audio_output_pcap_path),
                Some(CaseHostPostCheck::UsbAudioIso {
                    output_pcap_path: layout.usb_audio_output_pcap_path.clone(),
                }),
            )
        }
        None => (Vec::new(), None),
    };

    Ok((extra_qemu_args, host_post_check))
}

pub(crate) fn apply_grouped_qemu_config(qemu: &mut QemuConfig, case: &StarryQemuCase) {
    if !case.is_grouped() {
        return;
    }

    qemu.shell_init_cmd = Some(GROUPED_CASE_RUNNER_PATH.to_string());
    qemu.success_regex = vec![GROUPED_CASE_SUCCESS_REGEX.to_string()];
    if !qemu
        .fail_regex
        .iter()
        .any(|regex| regex == GROUPED_CASE_FAIL_REGEX)
    {
        qemu.fail_regex.push(GROUPED_CASE_FAIL_REGEX.to_string());
    }
}

pub(crate) fn write_grouped_case_runner_script(
    overlay_dir: &Path,
    test_commands: &[String],
) -> anyhow::Result<()> {
    ensure!(
        !test_commands.is_empty(),
        "grouped qemu case has no test commands"
    );

    let dest_dir = overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;
    let runner_path = dest_dir.join(GROUPED_CASE_RUNNER_NAME);

    let mut body = String::new();
    body.push_str("failed=0\n");
    for command in test_commands {
        let quoted = shell_single_quote(command);
        let begin = shell_single_quote(&format!("STARRY_GROUPED_TEST_BEGIN: {command}"));
        let passed = shell_single_quote(&format!("STARRY_GROUPED_TEST_PASSED: {command}"));
        let failed = shell_single_quote(&format!("STARRY_GROUPED_TEST_FAILED: {command}"));
        body.push_str(&format!(
            "printf '%s\\n' {begin}\nif sh -c {quoted}; then\n\tprintf '%s\\n' \
             {passed}\nelse\n\tstatus=$?\n\tprintf '%s status=%s\\n' {failed} \
             \"$status\"\n\tfailed=1\nfi\n"
        ));
    }
    body.push_str(
        "if [ \"$failed\" -eq 0 ]; then\n\techo STARRY_GROUPED_TESTS_PASSED\n\texit 0\nfi\necho \
         STARRY_GROUPED_TESTS_FAILED\nexit 1\n",
    );

    write_executable_script(&runner_path, &body)
}

/// Prepares overlay assets for a Starry shell-based test case.
pub(crate) fn prepare_sh_case_assets_sync(
    case: &StarryQemuCase,
    case_rootfs: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    let sh_dir = case_sh_source_dir(case);
    ensure!(
        sh_dir.is_dir(),
        "sh directory not found at `{}`",
        sh_dir.display()
    );

    reset_dir(&layout.overlay_dir)?;

    let dest_dir = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;

    let mut entries = fs::read_dir(&sh_dir)
        .with_context(|| format!("failed to read {}", sh_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", sh_dir.display()))?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = dest_dir.join(entry.file_name());
        fs::copy(&path, &dest)
            .with_context(|| format!("failed to copy {} to {}", path.display(), dest.display()))?;
        make_executable(&dest)?;
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

/// Returns the extra QEMU arguments used for the synthetic USB backing image.
pub(crate) fn usb_qemu_args(usb_stick_path: &Path) -> Vec<String> {
    vec![
        "-device".to_string(),
        "qemu-xhci,id=xhci".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,format=raw,file={},id=usbstick0",
            usb_stick_path.display()
        ),
        "-device".to_string(),
        "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
    ]
}

/// Returns the extra QEMU arguments used for USB audio ISO playback tests.
pub(crate) fn usb_audio_qemu_args(output_pcap_path: &Path) -> Vec<String> {
    vec![
        "-device".to_string(),
        "qemu-xhci,id=xhci".to_string(),
        "-audiodev".to_string(),
        "none,id=aud0".to_string(),
        "-device".to_string(),
        format!(
            "usb-audio,audiodev=aud0,pcap={},bus=xhci.0",
            output_pcap_path.display()
        ),
    ]
}

/// Validates host-side outputs produced by Starry case-specific QEMU assets.
pub(crate) fn validate_case_host_outputs(
    case: &StarryQemuCase,
    assets: &StarryCaseAssets,
) -> anyhow::Result<()> {
    match &assets.host_post_check {
        Some(CaseHostPostCheck::UsbAudioIso { output_pcap_path }) => {
            validate_usb_audio_iso_output(case, output_pcap_path)
        }
        None => Ok(()),
    }
}

fn validate_usb_audio_iso_output(
    case: &StarryQemuCase,
    output_pcap_path: &Path,
) -> anyhow::Result<()> {
    ensure!(
        output_pcap_path.is_file(),
        "usb-audio-iso output pcap `{}` was not created",
        output_pcap_path.display()
    );
    let metadata = fs::metadata(output_pcap_path)
        .with_context(|| format!("failed to stat {}", output_pcap_path.display()))?;
    ensure!(
        metadata.len() > 24,
        "usb-audio-iso output pcap `{}` is too small",
        output_pcap_path.display()
    );

    let reference_wav_path = case.case_dir.join(USB_AUDIO_REFERENCE_WAV_REL_PATH);
    ensure!(
        reference_wav_path.is_file(),
        "usb-audio-iso reference wav `{}` is missing",
        reference_wav_path.display()
    );

    let reference = parse_pcm_wave_file(&reference_wav_path)?;
    let output_payload = parse_usb_audio_iso_payloads(output_pcap_path)?;
    compare_pcm_payload(&reference, &output_payload)
}

fn parse_pcm_wave_file(path: &Path) -> anyhow::Result<ParsedPcmWave> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_pcm_wave_bytes(&bytes).with_context(|| format!("failed to parse {}", path.display()))
}

fn parse_pcm_wave_bytes(bytes: &[u8]) -> anyhow::Result<ParsedPcmWave> {
    ensure!(bytes.len() >= 12, "wav is too small");
    ensure!(&bytes[..4] == b"RIFF", "wav missing RIFF header");
    ensure!(&bytes[8..12] == b"WAVE", "wav missing WAVE signature");

    let mut cursor = 12usize;
    let mut fmt = None;
    let mut data = None;
    while cursor + 8 <= bytes.len() {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_len = le_u32(&bytes[cursor + 4..cursor + 8]) as usize;
        cursor += 8;
        let available_len = bytes.len().saturating_sub(cursor);
        let clamped_chunk_len = if chunk_len == 0 || chunk_len > available_len {
            available_len
        } else {
            chunk_len
        };

        match chunk_id {
            b"fmt " => {
                ensure!(clamped_chunk_len >= 16, "wav fmt chunk too small");
                let chunk = &bytes[cursor..cursor + clamped_chunk_len];
                let audio_format = le_u16(&chunk[..2]);
                ensure!(audio_format == 1, "wav is not PCM");
                fmt = Some(ParsedPcmWave {
                    channels: le_u16(&chunk[2..4]),
                    sample_rate: le_u32(&chunk[4..8]),
                    byte_rate: le_u32(&chunk[8..12]),
                    block_align: le_u16(&chunk[12..14]),
                    bits_per_sample: le_u16(&chunk[14..16]),
                    data: Vec::new(),
                });
            }
            b"data" => {
                data = Some(bytes[cursor..cursor + clamped_chunk_len].to_vec());
            }
            _ => {}
        }

        cursor += clamped_chunk_len;
        if clamped_chunk_len % 2 != 0 {
            cursor += 1;
        }
    }

    let mut fmt = fmt.context("wav missing fmt chunk")?;
    fmt.data = data.context("wav missing data chunk")?;
    Ok(fmt)
}

fn compare_pcm_payload(reference: &ParsedPcmWave, output_payload: &[u8]) -> anyhow::Result<()> {
    ensure!(
        output_payload.len() == reference.data.len(),
        "captured USB payload length {} does not match reference {}",
        output_payload.len(),
        reference.data.len()
    );
    ensure!(
        output_payload == reference.data.as_slice(),
        "captured USB payload differs from reference"
    );
    Ok(())
}

fn parse_usb_audio_iso_payloads(path: &Path) -> anyhow::Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    ensure!(bytes.len() >= 24, "pcap is too small");

    let magic = le_u32(&bytes[..4]);
    ensure!(
        magic == 0xa1b2c3d4 || magic == 0xd4c3b2a1,
        "unsupported pcap magic"
    );

    let mut cursor = 24usize;
    let mut payload = Vec::new();
    while cursor + 16 <= bytes.len() {
        let incl_len = le_u32(&bytes[cursor + 8..cursor + 12]) as usize;
        cursor += 16;
        ensure!(
            cursor + incl_len <= bytes.len(),
            "pcap record extends past EOF"
        );
        let record = &bytes[cursor..cursor + incl_len];
        cursor += incl_len;

        if record.len() < 64 {
            continue;
        }

        let event_type = record[8];
        let transfer_type = record[9];
        let endpoint_number = record[10];
        let data_flag = record[15];
        let len_cap = le_u32(&record[36..40]) as usize;

        if event_type != b'S' || transfer_type != 0 || (endpoint_number & 0x80) != 0 {
            continue;
        }
        if data_flag != 0 && data_flag != b'=' {
            continue;
        }

        let payload_len = if len_cap == 0 {
            record.len().saturating_sub(64)
        } else {
            len_cap.min(record.len().saturating_sub(64))
        };
        payload.extend_from_slice(&record[64..64 + payload_len]);
    }

    ensure!(
        !payload.is_empty(),
        "usb-audio-iso pcap did not contain iso OUT payloads"
    );
    Ok(payload)
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().unwrap())
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

/// Creates the empty backing image used for USB-related QEMU test assets.
pub(crate) fn create_usb_backing_image(path: &Path) -> anyhow::Result<()> {
    let file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.set_len(USB_STICK_IMAGE_SIZE)
        .with_context(|| format!("failed to size {}", path.display()))
}

/// Resets a directory to an empty existing state.
pub(crate) fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn write_executable_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -u\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    make_executable(path)
}

fn make_executable(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn fake_case(root: &Path, name: &str) -> StarryQemuCase {
        let case_dir = root.join("test-suit/starryos/normal").join(name);
        fs::create_dir_all(&case_dir).unwrap();
        StarryQemuCase {
            name: name.to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-aarch64.toml"),
            build_config_path: None,
            test_commands: Vec::new(),
            subcases: Vec::new(),
        }
    }

    #[test]
    fn resolve_target_dir_uses_workspace_target_directory() {
        let root = tempdir().unwrap();
        let dir = resolve_target_dir(root.path(), "x86_64-unknown-none").unwrap();

        assert_eq!(dir, root.path().join("target/x86_64-unknown-none"));
    }

    #[tokio::test]
    async fn prepare_case_assets_keeps_default_cases_plain() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&target_dir).unwrap();
        fs::create_dir_all(&rootfs_dir).unwrap();
        fs::write(rootfs_dir.join("rootfs-x86_64-alpine.img"), b"rootfs").unwrap();
        let case = fake_case(root.path(), "smoke");

        let assets = prepare_case_assets(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            &case,
            rootfs_dir.join("rootfs-x86_64-alpine.img"),
        )
        .await
        .unwrap();

        assert_eq!(
            assets.rootfs_path,
            rootfs_dir.join("rootfs-x86_64-alpine.img")
        );
        assert!(assets.extra_qemu_args.is_empty());
        assert!(assets.host_post_check.is_none());
        assert_eq!(fs::read(&assets.rootfs_path).unwrap(), b"rootfs");
        assert!(!target_dir.join("rootfs-x86_64-smoke.img").exists());
    }

    #[test]
    fn case_asset_layout_and_usb_qemu_args_use_stable_paths() {
        let root = tempdir().unwrap();
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();

        assert_eq!(
            layout.work_dir,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/starry-cases/usb")
        );
        assert_eq!(
            layout.usb_audio_output_pcap_path,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/starry-cases/usb/usb-audio-iso.pcap")
        );
        assert_eq!(
            usb_qemu_args(&layout.usb_stick_path),
            vec![
                "-device".to_string(),
                "qemu-xhci,id=xhci".to_string(),
                "-drive".to_string(),
                format!(
                    "if=none,format=raw,file={},id=usbstick0",
                    layout.usb_stick_path.display()
                ),
                "-device".to_string(),
                "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
            ]
        );
    }

    #[test]
    fn grouped_runner_script_runs_all_commands_and_reports_summary() {
        let root = tempdir().unwrap();
        let overlay = root.path().join("overlay");
        let commands = vec![
            "/usr/bin/alpha".to_string(),
            "/usr/bin/beta --flag".to_string(),
        ];

        write_grouped_case_runner_script(&overlay, &commands).unwrap();

        let runner = overlay.join("usr/bin/starry-run-case-tests");
        let content = fs::read_to_string(&runner).unwrap();
        assert!(content.contains("STARRY_GROUPED_TEST_BEGIN: /usr/bin/alpha"));
        assert!(content.contains("STARRY_GROUPED_TEST_FAILED: /usr/bin/beta --flag"));
        assert!(content.contains("STARRY_GROUPED_TESTS_PASSED"));
    }

    #[test]
    fn usb_audio_qemu_args_use_stable_paths() {
        let root = tempdir().unwrap();
        let layout = case_asset_layout(
            root.path(),
            "aarch64-unknown-none-softfloat",
            "usb-audio-iso",
        )
        .unwrap();

        assert_eq!(
            usb_audio_qemu_args(&layout.usb_audio_output_pcap_path),
            vec![
                "-device".to_string(),
                "qemu-xhci,id=xhci".to_string(),
                "-audiodev".to_string(),
                "none,id=aud0".to_string(),
                "-device".to_string(),
                format!(
                    "usb-audio,audiodev=aud0,pcap={},bus=xhci.0",
                    layout.usb_audio_output_pcap_path.display()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn prepare_case_assets_adds_usb_audio_host_check() {
        let root = tempdir().unwrap();
        let rootfs_dir = root.path().join("target/rootfs");
        fs::create_dir_all(&rootfs_dir).unwrap();
        let rootfs_path = rootfs_dir.join("rootfs-aarch64-alpine.img");
        fs::write(&rootfs_path, b"rootfs").unwrap();
        let case = fake_case(root.path(), "usb-audio-iso");

        let assets = prepare_case_assets(
            root.path(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
            &case,
            rootfs_path.clone(),
        )
        .await
        .unwrap();

        assert_eq!(assets.rootfs_path, rootfs_path);
        assert!(matches!(
            assets.host_post_check,
            Some(CaseHostPostCheck::UsbAudioIso { .. })
        ));
        assert!(
            assets
                .extra_qemu_args
                .iter()
                .any(|arg| arg.contains("usb-audio"))
        );
    }

    #[test]
    fn parse_usb_audio_iso_payloads_reads_submit_iso_out_records() {
        let mut pcap = Vec::new();
        pcap.extend_from_slice(&0xd4c3b2a1u32.to_le_bytes());
        pcap.extend_from_slice(&2u16.to_le_bytes());
        pcap.extend_from_slice(&4u16.to_le_bytes());
        pcap.extend_from_slice(&0i32.to_le_bytes());
        pcap.extend_from_slice(&0u32.to_le_bytes());
        pcap.extend_from_slice(&65_535u32.to_le_bytes());
        pcap.extend_from_slice(&249u32.to_le_bytes());

        let mut record = vec![0u8; 64];
        record[8] = b'S';
        record[9] = 0;
        record[10] = 1;
        record[15] = b'=';
        record[36..40].copy_from_slice(&4u32.to_le_bytes());
        record.extend_from_slice(&[1, 2, 3, 4]);

        pcap.extend_from_slice(&0u32.to_le_bytes());
        pcap.extend_from_slice(&0u32.to_le_bytes());
        pcap.extend_from_slice(&(record.len() as u32).to_le_bytes());
        pcap.extend_from_slice(&(record.len() as u32).to_le_bytes());
        pcap.extend_from_slice(&record);

        let root = tempdir().unwrap();
        let path = root.path().join("usb-audio-iso.pcap");
        fs::write(&path, pcap).unwrap();

        let payload = parse_usb_audio_iso_payloads(&path).unwrap();
        assert_eq!(payload, vec![1, 2, 3, 4]);
    }
}
