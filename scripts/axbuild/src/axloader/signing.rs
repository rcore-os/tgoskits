use std::path::{Path, PathBuf};

use anyhow::Context;

use super::{ArgsSign, sign_kernel};

/// An isolated signed artifact whose storage remains alive until upload ends.
pub(crate) struct SignedKernel {
    _directory: tempfile::TempDir,
    path: PathBuf,
}

impl SignedKernel {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Signs the final runtime ELF without changing the Cargo output or derived BIN.
/// The caller must keep the returned owner alive for the entire board session.
pub(crate) fn sign_runtime_kernel(
    workspace_root: &Path,
    elf: &Path,
    key: PathBuf,
) -> anyhow::Result<SignedKernel> {
    let directory = tempfile::tempdir().context("failed to create signed kernel directory")?;
    let path = directory
        .path()
        .join(elf.file_name().context("runtime ELF has no file name")?);
    sign_kernel(
        workspace_root,
        &ArgsSign {
            key,
            input: elf.to_owned(),
            output: path.clone(),
            // ostool's HTTP uploader selects this entry and reports the same
            // selector to axloader. The signature must bind that exact choice.
            entry_symbol: Some("httpboot_entry".into()),
        },
    )?;
    Ok(SignedKernel {
        _directory: directory,
        path,
    })
}
