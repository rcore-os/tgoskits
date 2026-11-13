use std::{fs, path::Path};

use anyhow::{Context, ensure};

use super::{
    layout::reset_dir,
    types::{CASE_SH_DIR_NAME, CaseAssetLayout, TestQemuCase},
};

/// Returns the shell-script source directory for a QEMU test case.
pub(crate) fn case_sh_source_dir(case: &TestQemuCase) -> std::path::PathBuf {
    case.case_dir.join(CASE_SH_DIR_NAME)
}

/// Prepares overlay assets for a shell-based QEMU test case.
pub(crate) fn prepare_sh_case_assets_sync(
    case: &TestQemuCase,
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

pub(super) fn write_executable_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -u\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    make_executable(path)
}

pub(super) fn make_executable(_path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(_path)
            .with_context(|| format!("failed to stat {}", _path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(_path, perms)
            .with_context(|| format!("failed to chmod {}", _path.display()))?;
    }
    Ok(())
}

pub(super) fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
