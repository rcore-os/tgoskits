//! Runtime dependency synchronization helpers for rootfs overlays.
//!
//! Main responsibilities:
//! - Scan overlay trees for ELF binaries that carry dynamic dependencies
//! - Resolve missing shared libraries from a staging root
//! - Copy those runtime dependencies into the overlay with preserved modes

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

const RUNTIME_LIBRARY_DIRS: &[&str] = &["lib", "usr/lib", "usr/local/lib"];

/// Copies any needed runtime shared libraries into an overlay tree.
pub(crate) fn sync_runtime_dependencies(
    staging_root: &Path,
    overlay_dir: &Path,
) -> anyhow::Result<()> {
    let readelf = find_host_binary_candidates(&["readelf"])?;
    let mut pending = collect_regular_files(overlay_dir)?;
    let mut processed = std::collections::BTreeSet::new();

    while let Some(path) = pending.pop() {
        if processed.contains(&path) || !is_elf_binary(&path)? {
            continue;
        }
        processed.insert(path.clone());

        let needed = read_needed_shared_libraries(&readelf, &path)?;
        for library in needed {
            let Some(source_path) = find_runtime_library_in_staging_root(staging_root, &library)?
            else {
                continue;
            };
            let relative_path = source_path
                .strip_prefix(staging_root)
                .with_context(|| format!("failed to relativize {}", source_path.display()))?;
            let overlay_path = overlay_dir.join(relative_path);
            if overlay_path.exists() {
                pending.push(overlay_path);
                continue;
            }

            if let Some(parent) = overlay_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }

            let resolved_source =
                fs::canonicalize(&source_path).unwrap_or_else(|_| source_path.clone());
            fs::copy(&resolved_source, &overlay_path).with_context(|| {
                format!(
                    "failed to copy runtime dependency {} to {}",
                    resolved_source.display(),
                    overlay_path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&resolved_source)
                    .with_context(|| format!("failed to stat {}", resolved_source.display()))?
                    .permissions()
                    .mode();
                fs::set_permissions(&overlay_path, fs::Permissions::from_mode(mode))
                    .with_context(|| format!("failed to chmod {}", overlay_path.display()))?;
            }
            pending.push(overlay_path);
        }
    }

    Ok(())
}

fn collect_regular_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.is_dir() {
        return Ok(files);
    }
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            files.extend(collect_regular_files(&path)?);
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(files)
}

fn read_needed_shared_libraries(readelf: &Path, binary: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new(readelf)
        .arg("-d")
        .arg(binary)
        .output()
        .with_context(|| {
            format!(
                "failed to run {} on {}",
                readelf.display(),
                binary.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "readelf failed for {} with status {}",
            binary.display(),
            output.status
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_needed_shared_library_line)
        .collect())
}

fn parse_needed_shared_library_line(line: &str) -> Option<String> {
    let marker = "Shared library: [";
    let start = line.find(marker)? + marker.len();
    let end = line[start..].find(']')?;
    Some(line[start..start + end].to_string())
}

fn is_elf_binary(path: &Path) -> anyhow::Result<bool> {
    let mut header = [0u8; 4];
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let read = file
        .read(&mut header)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(read == 4 && header == [0x7f, b'E', b'L', b'F'])
}

fn find_runtime_library_in_staging_root(
    staging_root: &Path,
    library: &str,
) -> anyhow::Result<Option<PathBuf>> {
    for relative_dir in RUNTIME_LIBRARY_DIRS {
        let candidate = staging_root.join(relative_dir).join(library);
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn find_host_binary_candidates(candidates: &[&str]) -> anyhow::Result<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_optional_host_binary(candidate))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "required host binary was not found in PATH; tried: {}",
                candidates.join(", ")
            )
        })
}

fn find_optional_host_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}
