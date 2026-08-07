//! OVMF image preparation for nested x86 Axvisor tests.

use std::{fs, io::Write, path::Path};

use anyhow::{Context, ensure};
use ostool::ovmf::Arch;
use tempfile::NamedTempFile;

const OVMF_SIZE: usize = 4 * 1024 * 1024;

pub(super) async fn prepare_x86_ovmf(output_path: &Path) -> anyhow::Result<()> {
    let firmware = crate::support::ovmf::OvmfFirmware::fetch(Arch::X64).await?;
    let code_path = firmware.code();
    let code = fs::read(code_path)
        .with_context(|| format!("failed to read OVMF code image {}", code_path.display()))?;
    let vars = if code.len() == OVMF_SIZE {
        None
    } else {
        OVMF_SIZE.checked_sub(code.len()).with_context(|| {
            format!(
                "OVMF code image {} is larger than 4 MiB",
                code_path.display()
            )
        })?;
        Some(fs::read(firmware.vars()).with_context(|| {
            format!(
                "failed to read OVMF variable store {}",
                firmware.vars().display()
            )
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
