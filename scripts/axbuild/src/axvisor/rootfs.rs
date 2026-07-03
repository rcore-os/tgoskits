//! Axvisor-specific rootfs resolution and preparation helpers.
//!
//! Main responsibilities:
//! - Resolve which rootfs image Axvisor should use for a QEMU run
//! - Distinguish between explicit, managed, and VM-config-derived rootfs paths
//! - Prepare managed rootfs images before launch when Axvisor relies on them
//! - Patch QEMU configs with the selected rootfs using Axvisor-specific rules

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use serde::Deserialize;

use super::{Axvisor, build};
use crate::{
    context::ResolvedAxvisorRequest,
    image::{config::ImageConfig, spec::ImageSpecRef, storage::Storage},
    rootfs,
    test::qemu as qemu_test,
};

const AXVISOR_GUEST_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/arceos-hypervisor/axvisor-guest/refs/heads/main/registry/default.toml";
const X86_LINUX_GUEST_KERNEL_PATH: &str = "/guest/linux/linux-qemu";
const X86_LINUX_TGOSIMAGE_NAME: &str = "qemu-x86_64";
const X86_LINUX_TGOSIMAGE_KERNEL_REL_PATHS: &[&str] =
    &["linux/linux-qemu", "linux-qemu", "qemu-x86_64"];
const X86_LINUX_AXVISOR_GUEST_IMAGE_NAME: &str = "qemu_x86_64_linux";
const X86_LINUX_AXVISOR_GUEST_KERNEL_REL_PATHS: &[&str] = &["linux-qemu", "qemu-x86_64"];

#[derive(Deserialize)]
struct VmRootfsProbe {
    kernel: Option<VmKernelRootfsProbe>,
}

#[derive(Deserialize)]
struct VmKernelRootfsProbe {
    image_location: Option<String>,
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
    ensure_qemu_rootfs_ready(
        &request,
        axvisor.app.workspace_root(),
        explicit_rootfs.as_deref(),
    )
    .await?;
    prepare_loongarch_linux_vmconfigs(
        &mut request,
        axvisor.app.workspace_root(),
        explicit_rootfs.as_deref(),
    )?;
    prepare_x86_linux_vmconfigs(&mut request, axvisor.app.workspace_root()).await?;
    let cargo = build::load_cargo_config(&request)?;
    let qemu =
        load_patched_qemu_config(axvisor, &request, &cargo, explicit_rootfs.as_deref()).await?;
    axvisor
        .app
        .qemu(cargo, request.build_info_path, Some(qemu))
        .await
}

pub(crate) fn prepare_loongarch_linux_vmconfigs(
    request: &mut ResolvedAxvisorRequest,
    workspace_root: &Path,
    _explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    if request.arch != "loongarch64" || request.vmconfigs.is_empty() {
        return Ok(());
    }

    let firmware_path = loongarch_uefi_firmware_path(workspace_root).ok_or_else(|| {
        anyhow!("LoongArch UEFI firmware image was not found; expected ostool OVMF code.fd")
    })?;
    let out_dir = workspace_root.join("tmp/axbuild/axvisor/loongarch64");
    let mut prepared_vmconfigs = Vec::with_capacity(request.vmconfigs.len());

    for vmconfig in &request.vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
        let guest_kernel = value
            .get("kernel")
            .and_then(|kernel| kernel.get("kernel_path"))
            .and_then(|path| path.as_str());

        if guest_kernel != Some("/guest/linux/linux-qemu") {
            prepared_vmconfigs.push(vmconfig.clone());
            continue;
        }

        let prepared_vmconfig = out_dir.join(
            vmconfig
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("linux-rootfs-smp1.toml")),
        );
        fs::create_dir_all(&out_dir)
            .with_context(|| format!("failed to create {}", out_dir.display()))?;
        let patched = replace_toml_string_value(
            &content,
            "uefi_firmware_path",
            &firmware_path.display().to_string(),
        );
        fs::write(&prepared_vmconfig, patched)
            .with_context(|| format!("failed to write {}", prepared_vmconfig.display()))?;
        prepared_vmconfigs.push(prepared_vmconfig);
    }

    request.vmconfigs = prepared_vmconfigs;
    Ok(())
}

pub(crate) async fn prepare_x86_linux_vmconfigs(
    request: &mut ResolvedAxvisorRequest,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    if request.arch != "x86_64" || request.vmconfigs.is_empty() {
        return Ok(());
    }

    if !x86_linux_vmconfigs_need_memory_kernel(&request.vmconfigs)? {
        return Ok(());
    }

    let guest_kernel_path = ensure_x86_linux_guest_kernel(workspace_root).await?;
    prepare_x86_linux_vmconfigs_from_kernel_path(request, workspace_root, &guest_kernel_path)
}

fn prepare_x86_linux_vmconfigs_from_kernel_path(
    request: &mut ResolvedAxvisorRequest,
    workspace_root: &Path,
    guest_kernel_path: &Path,
) -> anyhow::Result<()> {
    if request.arch != "x86_64" || request.vmconfigs.is_empty() {
        return Ok(());
    }

    let out_dir = workspace_root.join("tmp/axbuild/axvisor/x86_64");
    let mut prepared_vmconfigs = Vec::with_capacity(request.vmconfigs.len());

    for vmconfig in &request.vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        if !x86_linux_vmconfig_needs_memory_kernel(&content, vmconfig)? {
            prepared_vmconfigs.push(vmconfig.clone());
            continue;
        }

        let prepared_vmconfig = out_dir.join(
            vmconfig
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("linux-smp1.toml")),
        );
        fs::create_dir_all(&out_dir)
            .with_context(|| format!("failed to create {}", out_dir.display()))?;
        let kernel_path = guest_kernel_path.display().to_string();
        let patched = replace_toml_string_value(
            &replace_toml_string_value(&content, "image_location", "memory"),
            "kernel_path",
            &kernel_path,
        );
        fs::write(&prepared_vmconfig, patched)
            .with_context(|| format!("failed to write {}", prepared_vmconfig.display()))?;
        prepared_vmconfigs.push(prepared_vmconfig);
    }

    request.vmconfigs = prepared_vmconfigs;
    Ok(())
}

fn x86_linux_vmconfigs_need_memory_kernel(vmconfigs: &[PathBuf]) -> anyhow::Result<bool> {
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        if x86_linux_vmconfig_needs_memory_kernel(&content, vmconfig)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn x86_linux_vmconfig_needs_memory_kernel(content: &str, vmconfig: &Path) -> anyhow::Result<bool> {
    let probe: VmRootfsProbe = toml::from_str(content)
        .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
    let Some(kernel) = probe.kernel else {
        return Ok(false);
    };
    Ok(
        kernel.kernel_path.as_deref() == Some(X86_LINUX_GUEST_KERNEL_PATH)
            && kernel.image_location.as_deref() != Some("memory"),
    )
}

async fn ensure_x86_linux_guest_kernel(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let primary_config = ImageConfig::read_config(workspace_root)?;
    let primary_storage = Storage::new_from_config(&primary_config).await?;
    let mut errors = Vec::new();

    match pull_guest_kernel_from_storage(
        &primary_storage,
        X86_LINUX_TGOSIMAGE_NAME,
        X86_LINUX_TGOSIMAGE_KERNEL_REL_PATHS,
    )
    .await
    {
        Ok(path) => return Ok(path),
        Err(err) => errors.push(format!("{X86_LINUX_TGOSIMAGE_NAME}: {err:#}")),
    }

    match pull_guest_kernel_from_storage(
        &primary_storage,
        X86_LINUX_AXVISOR_GUEST_IMAGE_NAME,
        X86_LINUX_AXVISOR_GUEST_KERNEL_REL_PATHS,
    )
    .await
    {
        Ok(path) => return Ok(path),
        Err(err) => errors.push(format!("{X86_LINUX_AXVISOR_GUEST_IMAGE_NAME}: {err:#}")),
    }

    let mut fallback_config = primary_config;
    fallback_config.local_storage = fallback_config.local_storage.join("axvisor-guest");
    fallback_config.registry = AXVISOR_GUEST_REGISTRY_URL.to_string();
    let fallback_storage = Storage::new_from_config(&fallback_config).await?;
    match pull_guest_kernel_from_storage(
        &fallback_storage,
        X86_LINUX_AXVISOR_GUEST_IMAGE_NAME,
        X86_LINUX_AXVISOR_GUEST_KERNEL_REL_PATHS,
    )
    .await
    {
        Ok(path) => return Ok(path),
        Err(err) => errors.push(format!(
            "{X86_LINUX_AXVISOR_GUEST_IMAGE_NAME} from axvisor-guest registry: {err:#}"
        )),
    }

    bail!(
        "failed to prepare x86_64 Linux guest kernel image:\n{}",
        errors.join("\n")
    )
}

async fn pull_guest_kernel_from_storage(
    storage: &Storage,
    image_name: &str,
    kernel_rel_paths: &[&str],
) -> anyhow::Result<PathBuf> {
    let extract_dir = storage
        .pull_image(ImageSpecRef::parse(image_name), None, true)
        .await?;
    find_guest_kernel_path(&extract_dir, kernel_rel_paths)
}

fn find_guest_kernel_path(
    extract_dir: &Path,
    kernel_rel_paths: &[&str],
) -> anyhow::Result<PathBuf> {
    for kernel_rel_path in kernel_rel_paths {
        let kernel_path = extract_dir.join(kernel_rel_path);
        if kernel_path.is_file() {
            return Ok(kernel_path);
        }
    }

    bail!(
        "image extracted to {} but did not contain any of: {}",
        extract_dir.display(),
        kernel_rel_paths.join(", ")
    )
}

fn loongarch_uefi_firmware_path(workspace_root: &Path) -> Option<PathBuf> {
    [
        PathBuf::from("/tmp/ostool/ovmf/loongarch64/code.fd"),
        workspace_root.join("tmp/ostool/ovmf/loongarch64/code.fd"),
        workspace_root.join("tmp/loongarch-uefi-stage1/assets/qemu-binary/QEMU_EFI.fd"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn replace_toml_string_value(content: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key} = ");
    content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with(&prefix) {
                let indent_len = line.len() - line.trim_start().len();
                format!("{}{}\"{}\"", &line[..indent_len], prefix, value)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
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
    qemu_test::apply_dynamic_platform_qemu_boot(&mut qemu, cargo);
    Ok(qemu)
}

/// Ensures the managed rootfs required by an Axvisor QEMU run is available.
pub(crate) async fn ensure_qemu_rootfs_ready(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = managed_rootfs_path(request, workspace_root, explicit_rootfs)?;
    crate::image::storage::ensure_optional_managed_rootfs(
        workspace_root,
        &request.arch,
        rootfs_path.as_deref(),
    )
    .await
}

/// Patches a QEMU config with the rootfs selected for an Axvisor request.
pub(crate) fn patch_qemu_rootfs(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    patch_qemu_rootfs_path(config, &rootfs_path);
    Ok(())
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
pub(crate) fn patch_qemu_rootfs_path(config: &mut QemuConfig, rootfs_path: &Path) {
    rootfs::qemu::patch_rootfs(
        config,
        rootfs_path,
        rootfs::qemu::RootfsPatchMode::ReplaceDriveOnly,
    );
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
        let Some(kernel) = probe.kernel else {
            continue;
        };
        if kernel.image_location.as_deref() == Some("memory") {
            continue;
        }
        let Some(kernel_path) = kernel.kernel_path else {
            continue;
        };
        let rootfs_path = Path::new(&kernel_path)
            .parent()
            .map(|dir| dir.join("rootfs.img"));
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
    use tempfile::tempdir;

    use super::*;

    fn managed_rootfs_path_for_test(root: &Path, image_name: &str) -> PathBuf {
        root.join(".tgos-images").join(image_name).join(image_name)
    }

    fn write_test_image_config(root: &Path) {
        let config = crate::image::config::ImageConfig {
            local_storage: root.join(".tgos-images"),
            registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
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

    #[test]
    fn infer_rootfs_path_uses_vmconfig_kernel_sibling() {
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
    fn infer_rootfs_path_skips_memory_kernel_sibling_rootfs() {
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
image_location = "memory"
kernel_path = "{}"
"#,
                image_dir.join("linux-qemu").display()
            ),
        )
        .unwrap();

        assert_eq!(infer_rootfs_path(&[vmconfig]).unwrap(), None);
    }

    #[test]
    fn prepare_x86_linux_vmconfigs_patches_rootfs_guest_kernel_to_memory() {
        let root = tempdir().unwrap();
        let vmconfig = root.path().join("linux-svm-smp1.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
image_location = "fs"
kernel_path = "/guest/linux/linux-qemu"
"#,
        )
        .unwrap();
        let kernel_path = root.path().join("images/qemu-x86_64/linux/linux-qemu");
        let mut request = request(root.path(), vec![vmconfig.clone()]);
        request.arch = "x86_64".to_string();
        request.target = "x86_64-unknown-none".to_string();

        prepare_x86_linux_vmconfigs_from_kernel_path(&mut request, root.path(), &kernel_path)
            .unwrap();

        assert_ne!(request.vmconfigs, vec![vmconfig.clone()]);
        let prepared = request.vmconfigs.first().unwrap();
        assert_eq!(
            prepared,
            &root
                .path()
                .join("tmp/axbuild/axvisor/x86_64/linux-svm-smp1.toml")
        );
        let content = fs::read_to_string(prepared).unwrap();
        assert!(content.contains(r#"image_location = "memory""#));
        assert!(content.contains(&format!(r#"kernel_path = "{}""#, kernel_path.display())));
        let original = fs::read_to_string(vmconfig).unwrap();
        assert!(original.contains(r#"image_location = "fs""#));
        assert!(original.contains(r#"kernel_path = "/guest/linux/linux-qemu""#));
    }

    #[test]
    fn find_guest_kernel_path_uses_first_existing_candidate() {
        let root = tempdir().unwrap();
        let extract_dir = root.path().join("qemu_x86_64_linux");
        fs::create_dir_all(&extract_dir).unwrap();
        fs::write(extract_dir.join("qemu-x86_64"), b"kernel").unwrap();

        assert_eq!(
            find_guest_kernel_path(&extract_dir, &["linux-qemu", "qemu-x86_64"]).unwrap(),
            extract_dir.join("qemu-x86_64")
        );
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
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
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
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                rootfs_path.display()
            )]
        );
    }

    #[test]
    fn patch_qemu_rootfs_uses_unified_rootfs_by_default() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                rootfs.display()
            )]
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
                "virtio-blk-device,drive=disk0".to_string(),
                "-append".to_string(),
                "root=/dev/vda rw init=/bin/sh".to_string(),
            ],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-drive".to_string(),
                format!("id=disk0,if=none,format=raw,file={}", rootfs.display()),
                "-append".to_string(),
                "root=/dev/vda rw init=/bin/sh".to_string(),
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
}
