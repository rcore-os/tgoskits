//! Axvisor-specific rootfs resolution and preparation helpers.
//!
//! Main responsibilities:
//! - Resolve which rootfs image Axvisor should use for a QEMU run
//! - Distinguish between explicit, managed, and VM-config-derived rootfs paths
//! - Prepare managed rootfs and guest image bundles before launch
//! - Patch QEMU configs with the selected rootfs using Axvisor-specific rules

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use serde::Deserialize;

use super::{Axvisor, build};
use crate::{
    context::ResolvedAxvisorRequest,
    image::{config::ImageConfig, spec::ImageSpecRef, storage::Storage},
    rootfs,
};

#[derive(Deserialize)]
struct VmRootfsProbe {
    kernel: Option<VmKernelRootfsProbe>,
}

#[derive(Deserialize)]
struct VmKernelRootfsProbe {
    kernel_path: Option<String>,
    ramdisk_path: Option<String>,
}

pub(super) async fn qemu(axvisor: &mut Axvisor, args: super::ArgsQemu) -> anyhow::Result<()> {
    let mut request = axvisor.prepare_request(
        (&args.build).into(),
        args.qemu_config,
        None,
        crate::context::SnapshotPersistence::Store,
    )?;
    axvisor.app.set_debug_mode(request.debug)?;
    let explicit_rootfs = args
        .rootfs
        .map(|rootfs| {
            crate::image::storage::resolve_explicit_rootfs(
                axvisor.app.workspace_root(),
                axvisor.app.target_dir(),
                &request.arch,
                rootfs,
            )
        })
        .transpose()?;
    let mut cargo = build::load_cargo_config(&request, axvisor.app.workspace_context())?;
    request.vmconfigs = build::vmconfigs_from_cargo(&cargo);
    let qemu =
        load_patched_qemu_config(axvisor, &request, &cargo, explicit_rootfs.as_deref()).await?;
    if diskless_explicit_qemu(
        &qemu,
        request.qemu_config.is_some(),
        explicit_rootfs.is_some(),
    ) {
        ensure_guest_image_bundles(
            &request,
            axvisor.app.workspace_root(),
            axvisor.app.target_dir(),
        )
        .await?;
    } else {
        ensure_qemu_assets_ready(
            &request,
            axvisor.app.workspace_root(),
            axvisor.app.target_dir(),
            explicit_rootfs.as_deref(),
        )
        .await?;
    }
    cargo.to_bin = qemu_to_bin_requested(&qemu)?;
    axvisor
        .app
        .qemu(cargo, request.build_info_path, Some(qemu))
        .await
}

fn qemu_to_bin_requested(qemu: &QemuConfig) -> anyhow::Result<bool> {
    if qemu.uefi && !qemu.to_bin {
        bail!(
            "QEMU config enables UEFI but does not request `to_bin = true`; set `to_bin = true` \
             explicitly"
        );
    }
    Ok(qemu.to_bin)
}

pub(super) async fn load_patched_qemu_config(
    axvisor: &mut Axvisor,
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<QemuConfig> {
    let config_path = request.qemu_config.clone().unwrap_or_else(|| {
        super::default_qemu_config_template_path(&request.axvisor_dir, &request.arch)
    });
    let mut qemu = axvisor
        .app
        .read_qemu_config_from_path_for_cargo(cargo, &config_path)
        .await?;
    if !diskless_explicit_qemu(
        &qemu,
        request.qemu_config.is_some(),
        explicit_rootfs.is_some(),
    ) {
        patch_qemu_rootfs(
            &mut qemu,
            request,
            axvisor.app.workspace_root(),
            axvisor.app.target_dir(),
            explicit_rootfs,
        )?;
    }
    Ok(qemu)
}

pub(super) fn diskless_explicit_qemu(
    qemu: &QemuConfig,
    explicit_config: bool,
    explicit_rootfs: bool,
) -> bool {
    explicit_config
        && !explicit_rootfs
        && rootfs::qemu::host_initramfs_without_rootfs_drive(qemu)
        && !has_explicit_root(qemu)
}

fn has_explicit_root(qemu: &QemuConfig) -> bool {
    let has_root = |cmdline: &str| {
        let tokens = shlex::split(cmdline).unwrap_or_else(|| {
            cmdline
                .split_ascii_whitespace()
                .map(|token| token.trim_matches('"').to_owned())
                .collect()
        });
        tokens
            .into_iter()
            .take_while(|token| token != "--")
            .any(|token| {
                token
                    .strip_prefix("root=")
                    .is_some_and(|root| !root.is_empty())
            })
    };
    qemu.boot.cmdline.as_deref().is_some_and(has_root)
        || qemu
            .args
            .windows(2)
            .any(|pair| pair[0] == "-append" && has_root(&pair[1]))
        || qemu
            .args
            .iter()
            .filter_map(|argument| argument.strip_prefix("-append="))
            .any(has_root)
}

/// Ensures all image-managed assets required by an Axvisor QEMU run are available.
pub(crate) async fn ensure_qemu_assets_ready(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    target_dir: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    ensure_guest_image_bundles(request, workspace_root, target_dir).await?;
    let rootfs_path = managed_rootfs_path(request, workspace_root, target_dir, explicit_rootfs)?;
    crate::image::storage::ensure_optional_managed_rootfs(
        workspace_root,
        target_dir,
        &request.arch,
        rootfs_path.as_deref(),
    )
    .await
}

#[derive(Debug)]
struct GuestImageReference {
    vmconfig: PathBuf,
    required_path: PathBuf,
}

pub(super) async fn ensure_guest_image_bundles(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    target_dir: &Path,
) -> anyhow::Result<()> {
    let references = guest_image_references(&request.vmconfigs, workspace_root, target_dir)?;
    if references.is_empty() {
        return Ok(());
    }

    let output_dir = target_dir.join("axbuild").join("images");
    let mut config = ImageConfig::read_config(workspace_root, target_dir)?;
    // Guest VM configs use a stable workspace-relative path, while the new
    // image architecture keeps download and extraction ownership separate.
    // Reuse the configured archive cache but bind this operation's extracted
    // bundle output to the path referenced by the VM configs.
    config.extract_dir = output_dir.clone();
    let storage = Storage::new_from_config(&config).await?;
    for (image_name, references) in references {
        let spec = ImageSpecRef::parse(&image_name);
        let image = storage.resolve_image(spec).with_context(|| {
            format!("failed to resolve Axvisor guest image bundle `{image_name}`")
        })?;
        if image.arch != request.arch {
            bail!(
                "Axvisor guest image bundle `{image_name}` targets arch `{}`, expected `{}`",
                image.arch,
                request.arch
            );
        }
        let extracted = storage
            .pull_image(spec, true)
            .await
            .with_context(|| format!("failed to prepare Axvisor guest image `{image_name}`"))?;
        let expected_dir = output_dir.join(crate::image::storage::image_extract_dir_name(spec));
        if extracted != expected_dir {
            bail!(
                "Axvisor guest image path mismatch for `{image_name}`: expected {}, prepared {}",
                expected_dir.display(),
                extracted.display()
            );
        }

        for reference in references {
            if !reference.required_path.is_file() {
                bail!(
                    "Axvisor guest image `{image_name}` does not provide required file {} \
                     referenced by {}",
                    reference.required_path.display(),
                    reference.vmconfig.display()
                );
            }
        }
    }
    Ok(())
}

fn guest_image_references(
    vmconfigs: &[PathBuf],
    workspace_root: &Path,
    target_dir: &Path,
) -> anyhow::Result<BTreeMap<String, Vec<GuestImageReference>>> {
    let image_dir = target_dir.join("axbuild").join("images");
    let mut references = BTreeMap::<String, Vec<GuestImageReference>>::new();
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|error| anyhow!("failed to read vm config {}: {error}", vmconfig.display()))?;
        let probe: VmRootfsProbe = toml::from_str(&content).map_err(|error| {
            anyhow!("failed to parse vm config {}: {error}", vmconfig.display())
        })?;
        let Some(kernel) = probe.kernel else {
            continue;
        };
        for kernel_path in [kernel.kernel_path, kernel.ramdisk_path]
            .into_iter()
            .flatten()
        {
            let required_path =
                resolve_vm_asset_path(vmconfig, workspace_root, target_dir, &kernel_path);
            let Ok(relative) = required_path.strip_prefix(&image_dir) else {
                continue;
            };
            let components = relative.components().collect::<Vec<_>>();
            if components.is_empty()
                || components
                    .iter()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                bail!(
                    "invalid managed Axvisor guest image path `{}` in {}",
                    kernel_path,
                    vmconfig.display()
                );
            }
            let image_name = components[0]
                .as_os_str()
                .to_str()
                .ok_or_else(|| {
                    anyhow!(
                        "Axvisor guest image name in {} is not valid UTF-8",
                        vmconfig.display()
                    )
                })?
                .to_string();
            references
                .entry(image_name)
                .or_default()
                .push(GuestImageReference {
                    vmconfig: vmconfig.clone(),
                    required_path,
                });
        }
    }
    Ok(references)
}

fn resolve_vm_asset_path(
    vmconfig: &Path,
    workspace_root: &Path,
    target_dir: &Path,
    value: &str,
) -> PathBuf {
    let path = if let Some(relative) = value.strip_prefix("${workspace}/") {
        workspace_root.join(relative)
    } else {
        let path = Path::new(value);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            vmconfig
                .parent()
                .map_or_else(|| path.to_path_buf(), |parent| parent.join(path))
        }
    };
    crate::context::resolve_axbuild_artifact_path(workspace_root, target_dir, &path)
}

/// Patches a QEMU config with the rootfs selected for an Axvisor request.
pub(crate) fn patch_qemu_rootfs(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    target_dir: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = qemu_rootfs_path(request, workspace_root, target_dir, explicit_rootfs)?;
    let global_snapshot = config.args.iter().any(|argument| argument == "-snapshot");
    let write_policy = if global_snapshot {
        rootfs::qemu::RootfsWritePolicy::Discard
    } else {
        rootfs::qemu::RootfsWritePolicy::Persist
    };
    patch_qemu_rootfs_path(config, &rootfs_path, write_policy)?;
    // The shared rootfs patcher narrows Discard to the selected drive. Axvisor
    // must also retain the user's global protection for all other drives.
    if global_snapshot {
        config.args.push("-snapshot".into());
    }
    Ok(())
}

/// Resolves the rootfs path selected for an Axvisor QEMU request.
pub(crate) fn qemu_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    target_dir: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = explicit_rootfs {
        return Ok(explicit.to_path_buf());
    }

    infer_rootfs_path(&request.vmconfigs)?
        .map(Ok)
        .unwrap_or_else(|| {
            crate::image::storage::default_rootfs_path(workspace_root, target_dir, &request.arch)
        })
}

/// Patches a QEMU config with a concrete Axvisor rootfs path.
pub(crate) fn patch_qemu_rootfs_path(
    config: &mut QemuConfig,
    rootfs_path: &Path,
    write_policy: rootfs::qemu::RootfsWritePolicy,
) -> anyhow::Result<()> {
    if has_explicit_root(config) && !rootfs::qemu::has_host_rootfs_wiring(&config.args) {
        bail!("Axvisor root= requires a QEMU disk0 drive or device");
    }
    rootfs::qemu::patch_rootfs(
        config,
        rootfs_path,
        rootfs::qemu::RootfsPatchOptions {
            mode: rootfs::qemu::RootfsPatchMode::ReplaceDriveOnly,
            write_policy,
        },
    )
}

/// Returns the managed rootfs path Axvisor should prepare, if any.
pub(crate) fn managed_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    target_dir: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(explicit_rootfs) = explicit_rootfs {
        return crate::image::storage::resolve_managed_rootfs_path(
            workspace_root,
            target_dir,
            explicit_rootfs,
        );
    }

    if infer_rootfs_path(&request.vmconfigs)?.is_none() {
        return Ok(Some(crate::image::storage::default_rootfs_path(
            workspace_root,
            target_dir,
            &request.arch,
        )?));
    }

    Ok(None)
}

/// Infers a rootfs image path from VM config files by looking next to the
/// configured guest kernel image.
pub(crate) fn infer_rootfs_path(vmconfigs: &[PathBuf]) -> anyhow::Result<Option<PathBuf>> {
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        let probe: VmRootfsProbe = toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
        let Some(kernel_path) = probe.kernel.and_then(|kernel| kernel.kernel_path) else {
            continue;
        };
        let kernel_path = Path::new(&kernel_path);
        let kernel_path = if kernel_path.is_absolute() {
            kernel_path.to_path_buf()
        } else {
            vmconfig
                .parent()
                .map(|parent| parent.join(kernel_path))
                .unwrap_or_else(|| kernel_path.to_path_buf())
        };
        let rootfs_path = kernel_path.parent().map(|dir| dir.join("rootfs.img"));
        if let Some(rootfs_path) = rootfs_path
            && rootfs_path.exists()
        {
            return Ok(Some(rootfs_path));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;
    use crate::{image::registry::ImageEntry, support::download::test_support};

    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_data);
            for (name, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_path(name).unwrap();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append(&header, *contents).unwrap();
            }
            builder.finish().unwrap();
        }

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_data).unwrap();
        encoder.finish().unwrap()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn managed_rootfs_path_for_test(root: &Path, image_name: &str) -> PathBuf {
        root.join(".tgos-images").join(image_name)
    }

    fn target_dir_for_test(root: &Path) -> PathBuf {
        root.join("custom-target")
    }

    fn write_test_image_config(root: &Path) {
        let config = crate::image::config::ImageConfig {
            registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
            download_dir: root.join(".tgos-downloads"),
            extract_dir: root.join(".tgos-images"),
        };
        crate::image::config::ImageConfig::write_config(root, &config).unwrap();
    }

    fn request(root: &Path, vmconfigs: Vec<PathBuf>) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            smp: None,
            debug: false,
            build_info_path: root.join(".build.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs,
        }
    }

    #[tokio::test]
    async fn qemu_assets_prepare_guest_bundle_referenced_by_vm_config() {
        let root = tempdir().unwrap();
        let archive = make_tar_gz(&[("linux/linux-qemu", b"kernel"), ("linux/initrd", b"initrd")]);
        let archive_url = test_support::register_bytes("qemu-aarch64.tar.gz", archive.clone());
        let registry = crate::image::registry::ImageRegistry {
            images: vec![ImageEntry {
                name: "qemu-aarch64".to_string(),
                version: "0.0.1".to_string(),
                released_at: None,
                description: "QEMU AArch64 guest bundle".to_string(),
                sha256: sha256_hex(&archive),
                arch: "aarch64".to_string(),
                url: archive_url.url().to_string(),
            }],
        };
        let registry_url = test_support::register_text(
            "images.toml",
            toml::to_string(&registry).unwrap().into_bytes(),
        );
        crate::image::config::ImageConfig::write_config(
            root.path(),
            &crate::image::config::ImageConfig {
                registry: registry_url.url().to_string(),
                download_dir: root.path().join(".tgos-downloads"),
                extract_dir: root.path().join(".tgos-images"),
            },
        )
        .unwrap();

        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
kernel_path = "${workspace}/target/axbuild/images/qemu-aarch64/linux/linux-qemu"
ramdisk_path = "${workspace}/target/axbuild/images/qemu-aarch64/linux/initrd"
"#,
        )
        .unwrap();

        let target_dir = target_dir_for_test(root.path());
        let request = request(root.path(), vec![vmconfig]);
        ensure_guest_image_bundles(&request, root.path(), &target_dir)
            .await
            .unwrap();
        let guest_kernel = target_dir.join("axbuild/images/qemu-aarch64/linux/linux-qemu");
        let guest_initrd = target_dir.join("axbuild/images/qemu-aarch64/linux/initrd");
        assert_eq!(fs::read(&guest_kernel).unwrap(), b"kernel");
        assert_eq!(fs::read(&guest_initrd).unwrap(), b"initrd");

        let default_rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        fs::create_dir_all(default_rootfs.parent().unwrap()).unwrap();
        fs::write(&default_rootfs, b"rootfs").unwrap();
        ensure_qemu_assets_ready(&request, root.path(), &target_dir, None)
            .await
            .unwrap();

        assert_eq!(fs::read(guest_kernel).unwrap(), b"kernel");
        assert_eq!(fs::read(guest_initrd).unwrap(), b"initrd");
    }

    #[test]
    fn patch_qemu_rootfs_uses_unified_rootfs_by_default() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        let mut qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string(),
            ],
            ..Default::default()
        };

        patch_qemu_rootfs(
            &mut qemu,
            &request(root.path(), vec![]),
            root.path(),
            &target_dir_for_test(root.path()),
            None,
        )
        .unwrap();

        assert!(
            qemu.args
                .iter()
                .any(|arg| { arg.contains(&format!("file={}", rootfs.display())) })
        );
    }

    #[test]
    fn qemu_uefi_without_to_bin_is_rejected() {
        let qemu = QemuConfig {
            uefi: true,
            to_bin: false,
            ..Default::default()
        };

        assert!(qemu_to_bin_requested(&qemu).is_err());
    }

    #[test]
    fn explicit_diskless_qemu_keeps_host_rootfs_unattached() {
        let qemu = QemuConfig {
            args: vec!["-nographic".into()],
            ..Default::default()
        };
        assert!(!diskless_explicit_qemu(&qemu, true, false));
        let qemu = QemuConfig {
            boot: ostool::BootPayloadConfig {
                initramfs: Some("host.cpio".into()),
                ..Default::default()
            },
            ..qemu
        };
        assert!(diskless_explicit_qemu(&qemu, true, false));
        assert!(!diskless_explicit_qemu(&qemu, true, true));
        assert!(!diskless_explicit_qemu(&qemu, false, false));
        let mut with_guest_drive = qemu.clone();
        with_guest_drive.args = vec![
            "-drive".into(),
            "id=guestdisk,if=none,file=guest.img".into(),
        ];
        assert!(diskless_explicit_qemu(&with_guest_drive, true, false));
        with_guest_drive.boot.cmdline = Some("root=/dev/sda".into());
        assert!(!diskless_explicit_qemu(&with_guest_drive, true, false));
        assert!(
            patch_qemu_rootfs_path(
                &mut with_guest_drive,
                Path::new("rootfs.img"),
                rootfs::qemu::RootfsWritePolicy::Discard,
            )
            .unwrap_err()
            .to_string()
            .contains("requires a QEMU disk0")
        );
        with_guest_drive.boot.cmdline = Some("-- root=/dev/sda".into());
        assert!(diskless_explicit_qemu(&with_guest_drive, true, false));
        with_guest_drive.boot.cmdline = Some("\"root=/dev/sda\"".into());
        assert!(!diskless_explicit_qemu(&with_guest_drive, true, false));
        with_guest_drive.boot.cmdline = Some("root=\"\"".into());
        assert!(diskless_explicit_qemu(&with_guest_drive, true, false));
        with_guest_drive.boot.cmdline = Some("label=\"not root=/dev/sda\"".into());
        assert!(diskless_explicit_qemu(&with_guest_drive, true, false));
        with_guest_drive.boot.cmdline = None;
        with_guest_drive
            .args
            .extend(["-append".into(), "root=/dev/sda".into()]);
        assert!(!diskless_explicit_qemu(&with_guest_drive, true, false));
    }
}
