use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use ostool::variables::{VariableScope, expand_path_variables};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::context::ResolvedAxvisorRequest;

const IMAGE_PATH_FIELDS: [&str; 5] = [
    "kernel_path",
    "dtb_path",
    "bios_path",
    "uefi_firmware_path",
    "ramdisk_path",
];

pub(super) fn resolve_vmconfigs(
    request: &ResolvedAxvisorRequest,
    configured_paths: &[PathBuf],
) -> anyhow::Result<Vec<PathBuf>> {
    let workspace_root = super::workspace_root_from_axvisor_dir(&request.axvisor_dir);
    let scope = VariableScope::new(
        workspace_root.clone(),
        request.axvisor_dir.clone(),
        std::env::temp_dir(),
    );
    let output_dir = workspace_root.join("tmp/axbuild/axvisor/resolved-vm-configs");

    configured_paths
        .iter()
        .map(|path| {
            let source = if path.is_absolute() {
                path.clone()
            } else {
                workspace_root.join(path)
            };
            resolve_vmconfig(&source, &scope, &output_dir)
        })
        .collect()
}

fn resolve_vmconfig(
    source: &Path,
    scope: &VariableScope,
    output_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let content = fs::read_to_string(source)
        .with_context(|| format!("failed to read VM config {}", source.display()))?;
    let mut document = content
        .parse::<toml::Table>()
        .with_context(|| format!("failed to parse VM config {}", source.display()))?;
    let Some(kernel) = document.get("kernel").and_then(toml::Value::as_table) else {
        return Ok(source.to_path_buf());
    };

    let mut paths = Vec::new();
    let mut expanded_any = false;
    for field in IMAGE_PATH_FIELDS {
        let Some(value) = kernel.get(field) else {
            continue;
        };
        let value = value.as_str().ok_or_else(|| {
            anyhow!(
                "VM config {} field kernel.{field} must be a string",
                source.display()
            )
        })?;
        let expanded = expand_path_variables(Path::new(value), scope).with_context(|| {
            format!(
                "failed to expand VM config {} field kernel.{field}",
                source.display()
            )
        })?;
        expanded_any |= expanded != Path::new(value);
        paths.push((field, expanded));
    }

    if !expanded_any {
        return Ok(source.to_path_buf());
    }

    let source_dir = source
        .parent()
        .with_context(|| format!("VM config path has no parent: {}", source.display()))?;
    let kernel = document
        .get_mut("kernel")
        .and_then(toml::Value::as_table_mut)
        .expect("kernel table was validated above");
    for (field, path) in paths {
        let path = if path.is_absolute() {
            path
        } else {
            source_dir.join(path)
        };
        kernel.insert(
            field.to_string(),
            toml::Value::String(path.to_string_lossy().into_owned()),
        );
    }

    let resolved = toml::to_string_pretty(&document)
        .context("failed to serialize resolved Axvisor VM config")?;
    install_resolved_config(source, output_dir, &resolved)
}

fn install_resolved_config(
    source: &Path,
    output_dir: &Path,
    resolved: &str,
) -> anyhow::Result<PathBuf> {
    let digest = Sha256::digest(source.as_os_str().as_encoded_bytes());
    let output = output_dir.join(format!("{digest:x}.toml"));
    if fs::read_to_string(&output).is_ok_and(|current| current == resolved) {
        return Ok(output);
    }

    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create resolved VM config directory {}",
            output_dir.display()
        )
    })?;
    let mut temporary = NamedTempFile::new_in(output_dir).with_context(|| {
        format!(
            "failed to create temporary VM config in {}",
            output_dir.display()
        )
    })?;
    temporary
        .write_all(resolved.as_bytes())
        .with_context(|| format!("failed to write resolved VM config {}", output.display()))?;
    temporary
        .persist(&output)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install resolved VM config {}", output.display()))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn resolves_all_image_paths_from_one_variable_scope() {
        let root = tempdir().unwrap();
        let workspace = root.path();
        let package = workspace.join("os/axvisor");
        let source_dir = workspace.join("configs/vms");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("guest.toml");
        fs::write(
            &source,
            r#"
[kernel]
kernel_path = "${workspace}/tmp/kernel"
dtb_path = "${tmpDir}/guest.dtb"
bios_path = "firmware/bios.fd"
uefi_firmware_path = "${package}/firmware/uefi.fd"
ramdisk_path = "../images/initramfs.cpio"
"#,
        )
        .unwrap();
        let scope = VariableScope::new(
            workspace.to_path_buf(),
            package.clone(),
            std::env::temp_dir(),
        );

        let output = resolve_vmconfig(
            &source,
            &scope,
            &workspace.join("tmp/axbuild/axvisor/resolved-vm-configs"),
        )
        .unwrap();
        let resolved = fs::read_to_string(output)
            .unwrap()
            .parse::<toml::Table>()
            .unwrap();
        let kernel = resolved["kernel"].as_table().unwrap();
        let expected = [
            ("kernel_path", workspace.join("tmp/kernel")),
            ("dtb_path", std::env::temp_dir().join("guest.dtb")),
            ("bios_path", source_dir.join("firmware/bios.fd")),
            ("uefi_firmware_path", package.join("firmware/uefi.fd")),
            ("ramdisk_path", source_dir.join("../images/initramfs.cpio")),
        ];

        for (field, path) in expected {
            assert_eq!(
                kernel[field].as_str(),
                Some(path.to_string_lossy().as_ref())
            );
        }
    }
}
