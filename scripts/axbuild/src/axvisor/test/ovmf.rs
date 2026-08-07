//! OVMF image preparation for nested x86 Axvisor tests.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use tempfile::NamedTempFile;

const OVMF_SOURCE_ENV: &str = "AXVISOR_X86_64_UEFI_FIRMWARE";
const OVMF_SIZE: usize = 4 * 1024 * 1024;
const CODE_CANDIDATES: &[&str] = &[
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
    "/usr/share/edk2/ovmf/OVMF_CODE_4M.fd",
];
const VARS_CANDIDATES: &[&str] = &[
    "/usr/share/OVMF/OVMF_VARS_4M.fd",
    "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
    "/usr/share/edk2/ovmf/OVMF_VARS_4M.fd",
];

pub(super) fn prepare_x86_ovmf(output_path: &Path) -> anyhow::Result<()> {
    let configured = std::env::var_os(OVMF_SOURCE_ENV).map(PathBuf::from);
    let managed_code = std::env::temp_dir().join("ostool/ovmf/x64/code.fd");
    let code_path = configured
        .as_deref()
        .filter(|path| path.is_file())
        .or_else(|| managed_code.is_file().then_some(managed_code.as_path()))
        .or_else(|| {
            CODE_CANDIDATES
                .iter()
                .map(Path::new)
                .find(|path| path.is_file())
        })
        .with_context(|| {
            format!(
                "x86 Axvisor test requires an OVMF 4 MiB code image; set {OVMF_SOURCE_ENV} or \
                 install ovmf"
            )
        })?;
    let code = fs::read(code_path)
        .with_context(|| format!("failed to read OVMF code image {}", code_path.display()))?;
    let vars = if code.len() == OVMF_SIZE {
        None
    } else {
        let required = OVMF_SIZE.checked_sub(code.len()).with_context(|| {
            format!(
                "OVMF code image {} is larger than 4 MiB",
                code_path.display()
            )
        })?;
        let vars_path = paired_vars_path(code_path)
            .filter(|path| file_has_size(path, required))
            .or_else(|| {
                VARS_CANDIDATES
                    .iter()
                    .map(Path::new)
                    .find(|path| file_has_size(path, required))
                    .map(Path::to_path_buf)
            })
            .with_context(|| {
                format!(
                    "OVMF code image {} needs a {required:#x}-byte variable-store prefix",
                    code_path.display()
                )
            })?;
        Some(fs::read(&vars_path).with_context(|| {
            format!("failed to read OVMF variable store {}", vars_path.display())
        })?)
    };
    let image = assemble_ovmf_image(&code, vars.as_deref())?;

    let parent = output_path
        .parent()
        .with_context(|| format!("OVMF output path has no parent: {}", output_path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create OVMF output directory {}",
            parent.display()
        )
    })?;
    let mut temporary = NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary OVMF image in {}",
            parent.display()
        )
    })?;
    temporary
        .write_all(&image)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    temporary
        .persist(output_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to install {}", output_path.display()))?;
    Ok(())
}

fn assemble_ovmf_image(code: &[u8], vars: Option<&[u8]>) -> anyhow::Result<Vec<u8>> {
    ensure!(
        code.len() <= OVMF_SIZE,
        "OVMF code image is larger than 4 MiB"
    );
    let prefix_len = OVMF_SIZE - code.len();
    let vars = vars.unwrap_or_default();
    ensure!(
        vars.len() == prefix_len,
        "OVMF variable-store prefix is {:#x} bytes, expected {prefix_len:#x}",
        vars.len()
    );

    let mut image = Vec::with_capacity(OVMF_SIZE);
    image.extend_from_slice(vars);
    image.extend_from_slice(code);
    Ok(image)
}

fn paired_vars_path(code_path: &Path) -> Option<PathBuf> {
    let file_name = code_path.file_name()?.to_str()?;
    let vars_name = if file_name == "code.fd" {
        "vars.fd".to_string()
    } else {
        file_name.replace("CODE", "VARS")
    };
    (vars_name != file_name).then(|| code_path.with_file_name(vars_name))
}

fn file_has_size(path: &Path, size: usize) -> bool {
    path.is_file() && fs::metadata(path).is_ok_and(|metadata| metadata.len() == size as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ovmf_is_assembled_at_the_top_of_the_four_mib_window() {
        let vars = vec![0xa5; 0x84_000];
        let code = vec![0x5a; OVMF_SIZE - vars.len()];
        let image = assemble_ovmf_image(&code, Some(&vars)).unwrap();

        assert_eq!(image.len(), OVMF_SIZE);
        assert_eq!(&image[..vars.len()], vars);
        assert_eq!(&image[vars.len()..], code);
        assert!(assemble_ovmf_image(&code, None).is_err());
    }
}
