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
                &request.arch,
                rootfs,
            )
        })
        .transpose()?;
    let mut cargo = build::load_cargo_config(&request)?;
    request.vmconfigs = build::vmconfigs_from_cargo(&cargo);
    ensure_qemu_assets_ready(
        &request,
        axvisor.app.workspace_root(),
        explicit_rootfs.as_deref(),
    )
    .await?;
    let qemu =
        load_patched_qemu_config(axvisor, &request, &cargo, explicit_rootfs.as_deref()).await?;
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
    patch_qemu_rootfs(
        &mut qemu,
        request,
        axvisor.app.workspace_root(),
        explicit_rootfs,
    )?;
    Ok(qemu)
}

/// Ensures all image-managed assets required by an Axvisor QEMU run are available.
pub(crate) async fn ensure_qemu_assets_ready(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    ensure_guest_image_bundles(request, workspace_root).await?;
    let rootfs_path = managed_rootfs_path(request, workspace_root, explicit_rootfs)?;
    crate::image::storage::ensure_optional_managed_rootfs(
        workspace_root,
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

async fn ensure_guest_image_bundles(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    let references = guest_image_references(&request.vmconfigs, workspace_root)?;
    if references.is_empty() {
        return Ok(());
    }

    let output_dir = crate::context::axbuild_tmp_dir(workspace_root).join("images");
    let mut config = ImageConfig::read_config(workspace_root)?;
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
) -> anyhow::Result<BTreeMap<String, Vec<GuestImageReference>>> {
    let image_dir = crate::context::axbuild_tmp_dir(workspace_root).join("images");
    let mut references = BTreeMap::<String, Vec<GuestImageReference>>::new();
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|error| anyhow!("failed to read vm config {}: {error}", vmconfig.display()))?;
        let probe: VmRootfsProbe = toml::from_str(&content).map_err(|error| {
            anyhow!("failed to parse vm config {}: {error}", vmconfig.display())
        })?;
        let Some(kernel_path) = probe.kernel.and_then(|kernel| kernel.kernel_path) else {
            continue;
        };
        let required_path = resolve_vm_asset_path(vmconfig, workspace_root, &kernel_path);
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
    Ok(references)
}

fn resolve_vm_asset_path(vmconfig: &Path, workspace_root: &Path, value: &str) -> PathBuf {
    if let Some(relative) = value.strip_prefix("${workspace}/") {
        return workspace_root.join(relative);
    }
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        vmconfig
            .parent()
            .map_or_else(|| path.to_path_buf(), |parent| parent.join(path))
    }
}

/// Patches a QEMU config with the rootfs selected for an Axvisor request.
pub(crate) fn patch_qemu_rootfs(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    patch_qemu_rootfs_path(
        config,
        &rootfs_path,
        rootfs::qemu::RootfsWritePolicy::Persist,
    )
}

/// Resolves the rootfs path selected for an Axvisor QEMU request.
pub(crate) fn qemu_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = explicit_rootfs {
        return Ok(explicit.to_path_buf());
    }

    infer_rootfs_path(&request.vmconfigs)?
        .map(Ok)
        .unwrap_or_else(|| {
            crate::image::storage::default_rootfs_path(workspace_root, &request.arch)
        })
}

/// Patches a QEMU config with a concrete Axvisor rootfs path.
pub(crate) fn patch_qemu_rootfs_path(
    config: &mut QemuConfig,
    rootfs_path: &Path,
    write_policy: rootfs::qemu::RootfsWritePolicy,
) -> anyhow::Result<()> {
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
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(explicit_rootfs) = explicit_rootfs {
        return crate::image::storage::resolve_managed_rootfs_path(workspace_root, explicit_rootfs);
    }

    if infer_rootfs_path(&request.vmconfigs)?.is_none() {
        return Ok(Some(crate::image::storage::default_rootfs_path(
            workspace_root,
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
        let archive = make_tar_gz(&[("linux/linux-qemu", b"kernel")]);
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

        let default_rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        fs::create_dir_all(default_rootfs.parent().unwrap()).unwrap();
        fs::write(&default_rootfs, b"rootfs").unwrap();

        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
kernel_path = "${workspace}/tmp/axbuild/images/qemu-aarch64/linux/linux-qemu"
"#,
        )
        .unwrap();

        ensure_qemu_assets_ready(&request(root.path(), vec![vmconfig]), root.path(), None)
            .await
            .unwrap();

        assert_eq!(
            fs::read(
                root.path()
                    .join("tmp/axbuild/images/qemu-aarch64/linux/linux-qemu"),
            )
            .unwrap(),
            b"kernel"
        );
    }

    #[test]
    fn infer_rootfs_path_uses_vmconfig_kernel_sibling() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
kernel_path = "image/qemu-aarch64"
"#,
        )
        .unwrap();

        assert_eq!(
            infer_rootfs_path(&[vmconfig]).unwrap(),
            Some(image_dir.join("rootfs.img"))
        );
    }

    #[test]
    fn infer_rootfs_path_skips_vmconfig_without_kernel_path() {
        let root = tempdir().unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
cmdline = "console=ttyS0"
"#,
        )
        .unwrap();

        assert_eq!(infer_rootfs_path(&[vmconfig]).unwrap(), None);
    }

    #[test]
    fn infer_rootfs_path_skips_nonexistent_kernel_sibling_rootfs() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(infer_rootfs_path(&[vmconfig]).unwrap(), None);
    }

    #[test]
    fn patch_qemu_rootfs_overrides_rootfs_when_vmconfig_provides_one() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        let rootfs_path = image_dir.join("rootfs.img");
        fs::write(&rootfs_path, b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        let mut qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string(),
            ],
            ..Default::default()
        };
        patch_qemu_rootfs(
            &mut qemu,
            &request(root.path(), vec![vmconfig]),
            root.path(),
            None,
        )
        .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-drive".to_string(),
                format!("id=disk0,if=none,format=raw,file={}", rootfs_path.display())
            ]
        );
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

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-drive".to_string(),
                format!("id=disk0,if=none,format=raw,file={}", rootfs.display())
            ]
        );
    }

    #[test]
    fn patch_qemu_rootfs_inserts_drive_arg_when_template_omits_it() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
                "-append".to_string(),
                "root=/dev/nvme0n1 rw init=/bin/sh".to_string(),
            ],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "nvme,drive=disk0,serial=tgoskits,max_ioqpairs=64,msix_qsize=65".to_string(),
                "-drive".to_string(),
                format!("id=disk0,if=none,format=raw,file={}", rootfs.display()),
                "-append".to_string(),
                "root=/dev/nvme0n1 rw init=/bin/sh".to_string(),
            ]
        );
    }

    #[test]
    fn managed_rootfs_path_uses_default_unified_rootfs_when_vmconfig_has_no_rootfs() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
kernel_path = "/tmp/qemu-aarch64"
"#,
        )
        .unwrap();

        assert_eq!(
            managed_rootfs_path(&request(root.path(), vec![vmconfig]), root.path(), None).unwrap(),
            Some(managed_rootfs_path_for_test(
                root.path(),
                "rootfs-aarch64-alpine.img"
            ))
        );
    }

    #[test]
    fn managed_rootfs_path_skips_when_vmconfig_provides_kernel_sibling_rootfs() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(
            managed_rootfs_path(&request(root.path(), vec![vmconfig]), root.path(), None).unwrap(),
            None
        );
    }

    #[test]
    fn managed_rootfs_path_keeps_explicit_managed_rootfs() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let explicit = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-debian.img");

        assert_eq!(
            managed_rootfs_path(
                &request(root.path(), vec![]),
                root.path(),
                Some(explicit.as_path())
            )
            .unwrap(),
            Some(explicit)
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
    fn axvisor_host_rootfs_configs_use_nvme_device_names() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let configs = [
            "test-suit/axvisor/normal/qemu/smoke/qemu-aarch64.toml",
            "test-suit/axvisor/normal/qemu/smoke/qemu-riscv64.toml",
            "test-suit/axvisor/normal/qemu/build-loongarch64-unknown-none-softfloat.toml",
            "os/axvisor/configs/qemu/qemu-aarch64.toml",
            "os/axvisor/configs/qemu/qemu-riscv64.toml",
            "os/axvisor/configs/board/qemu-loongarch64.toml",
        ];

        for relative in configs {
            let config = fs::read_to_string(workspace_root.join(relative)).unwrap();
            assert!(
                !config.contains("root=/dev/vda"),
                "{relative} still names the removed VirtIO block root device"
            );
            assert!(
                config.contains("ax-driver/nvme") || config.contains("\"nvme,drive=disk0"),
                "{relative} does not enable or attach NVMe"
            );
        }
    }
}
