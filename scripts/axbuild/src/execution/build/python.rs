use super::*;

pub(crate) fn case_python_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join("python")
}

/// Prepares overlay assets for a Python-based QEMU test case.
///
/// This pipeline reuses the same staging rootfs and prebuild infrastructure as
/// the C pipeline, but instead of running CMake it:
/// 1. Installs `python3` via `apk add` inside the staging rootfs
/// 2. Copies `.py` files from the test's `python/` directory into `/usr/bin/`
/// 3. Builds an overlay from the Python installation and test scripts
/// 4. Injects the overlay into the rootfs image
pub(crate) fn prepare_python_case_assets_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    let python_dir = case_python_source_dir(case);
    ensure!(
        python_dir.is_dir(),
        "missing case Python source directory `{}`",
        python_dir.display()
    );

    case_assets::reset_dir(&layout.staging_root)?;
    case_assets::reset_dir(&layout.overlay_dir)?;
    case_assets::reset_dir(&layout.command_wrapper_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    // Extract rootfs into staging
    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
    (config.prepare_staging_root)(&layout.staging_root)?;
    write_musl_loader_search_path(arch, &layout.staging_root)?;

    // Prepare guest prebuild environment and install python3
    let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;

    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_guest_command_wrappers(layout, &qemu_runner)?;

    // Build the prebuild command: run "apk add python3" inside the staging rootfs
    let guest_busybox = layout.staging_root.join("bin/busybox");
    let guest_shell = layout.staging_root.join("bin/sh");
    let mut prebuild_cmd = Command::new(&qemu_runner);
    prebuild_cmd.arg("-L").arg(&layout.staging_root);
    if guest_busybox.is_file() {
        prebuild_cmd.arg(&guest_busybox).arg("sh");
    } else {
        ensure!(
            guest_shell.is_file(),
            "staging root is missing guest shell `{}`",
            guest_shell.display()
        );
        prebuild_cmd.arg(&guest_shell);
    }
    prebuild_cmd.arg("-eu").arg("-c").arg("apk add python3");

    // Apply environment (PATH with wrappers, guest dynamic linker paths)
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = Vec::new();
    path_entries.push(layout.command_wrapper_dir.clone());
    path_entries.extend(std::env::split_paths(&host_path));
    let path = std::env::join_paths(path_entries)
        .map_err(|e| anyhow::anyhow!("failed to build guest prebuild PATH: {e}"))?;
    prebuild_cmd.env("PATH", path);
    prebuild_cmd.env("QEMU_LD_PREFIX", &layout.staging_root);
    prebuild_cmd.env("LD_LIBRARY_PATH", guest_library_path(&layout.staging_root));
    for (key, value) in extra_script_envs {
        prebuild_cmd.env(key, value);
    }

    prebuild_cmd
        .exec()
        .context("failed to install python3 via apk in staging rootfs")?;

    // Build overlay: copy Python installation from staging into overlay
    let python_dirs_to_copy: &[&str] = &["usr/bin", "usr/lib", "lib"];

    for rel_dir in python_dirs_to_copy {
        let src = layout.staging_root.join(rel_dir);
        let dst = layout.overlay_dir.join(rel_dir);
        if src.is_dir() {
            copy_dir_recursive(&src, &dst, &layout.staging_root)
                .with_context(|| format!("failed to copy {} to overlay", src.display()))?;
        }
    }

    // Copy .py test files into overlay at /usr/bin/
    let dest_bin = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_bin)
        .with_context(|| format!("failed to create {}", dest_bin.display()))?;

    for entry in fs::read_dir(&python_dir)
        .with_context(|| format!("failed to read {}", python_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let dest = dest_bin.join(entry.file_name());
        fs::copy(&path, &dest)
            .with_context(|| format!("failed to copy {} to {}", path.display(), dest.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dest)
                .with_context(|| format!("failed to stat {}", dest.display()))?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dest, perms)
                .with_context(|| format!("failed to chmod {}", dest.display()))?;
        }
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

/// Writes a musl loader search-path file into the staging root so that the
/// guest dynamic linker (`ld-musl-{arch}.so.1`) finds libraries under `/usr/lib`
/// and `/lib` at runtime.
///
/// The file is written to `/etc/ld-musl-{arch}.path` only when the
/// corresponding loader binary is present under `lib/`; otherwise this is a
/// no-op (the rootfs may not ship musl at all, or may use a different libc).
///
/// This is called from `prepare_c_case_assets_sync`,
/// `prepare_python_case_assets_sync`, and `prepare_grouped_case_assets_sync`
/// because those pipelines extract a staging rootfs, optionally run a
/// `prebuild.sh`, and cross-build guest binaries that link against musl.
///
/// It is *not* called from `prepare_sh_case_assets_sync`: shell test cases do
/// not extract a staging rootfs, do not cross-build C guest binaries, and do
/// not have a `prebuild.sh` phase — they only copy scripts from the case's
/// `sh/` source directory into the overlay and inject them directly.
pub(super) fn write_musl_loader_search_path(arch: &str, staging_root: &Path) -> anyhow::Result<()> {
    let loader_path = staging_root
        .join("lib")
        .join(format!("ld-musl-{arch}.so.1"));
    if !loader_path.is_file() {
        return Ok(());
    }
    let etc_dir = staging_root.join("etc");
    fs::create_dir_all(&etc_dir)
        .with_context(|| format!("failed to create {}", etc_dir.display()))?;

    let path_file = etc_dir.join(format!("ld-musl-{arch}.path"));
    fs::write(&path_file, "/usr/lib\n/lib\n")
        .with_context(|| format!("failed to write {}", path_file.display()))?;

    Ok(())
}

/// Recursively copies a directory tree, preserving file permissions.
///
/// `allowed_root` is the canonical boundary: any symlink that resolves outside
/// this directory is rejected to prevent host filesystem leaks.
pub(super) fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    allowed_root: &Path,
) -> anyhow::Result<()> {
    let canonical_root = fs::canonicalize(allowed_root).with_context(|| {
        format!(
            "failed to canonicalize allowed root {}",
            allowed_root.display()
        )
    })?;
    copy_dir_recursive_inner(src, dst, &canonical_root)
}

/// Inner recursive implementation that operates on an already-canonical root.
/// This avoids re-canonicalizing `allowed_root` on every recursion level.
pub(super) fn copy_dir_recursive_inner(
    src: &Path,
    dst: &Path,
    canonical_root: &Path,
) -> anyhow::Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", src_path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive_inner(&src_path, &dst_path, canonical_root)?;
        } else if file_type.is_symlink() {
            // For symlinks: read the link target to decide what to do.
            let link_target = fs::read_link(&src_path)
                .with_context(|| format!("failed to read symlink {}", src_path.display()))?;

            // Resolve the symlink target within the guest rootfs, not the host.
            // Absolute guest symlinks (e.g. /bin/busybox) must be rebased onto
            // canonical_root before canonicalization; otherwise fs::canonicalize
            // would follow them through the host's "/" and produce a path that
            // trivially escapes the staging root.
            let host_target = if link_target.is_absolute() {
                // Strip the leading "/" so Path::join doesn't discard canonical_root.
                let rel = link_target.strip_prefix("/").unwrap_or(&link_target);
                canonical_root.join(rel)
            } else {
                // Relative symlink: resolve from the directory that contains it.
                src_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(&link_target)
            };

            match fs::canonicalize(&host_target) {
                Ok(resolved) => {
                    // Symlink resolves — verify it stays within the staging root.
                    ensure!(
                        resolved.starts_with(canonical_root),
                        "symlink `{}` resolves to `{}` which escapes the staging root `{}`",
                        src_path.display(),
                        resolved.display(),
                        canonical_root.display()
                    );
                    if resolved.is_file() {
                        fs::copy(&resolved, &dst_path).with_context(|| {
                            format!(
                                "failed to copy {} to {}",
                                resolved.display(),
                                dst_path.display()
                            )
                        })?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let mode = fs::metadata(&resolved)
                                .with_context(|| format!("failed to stat {}", resolved.display()))?
                                .permissions()
                                .mode();
                            fs::set_permissions(&dst_path, fs::Permissions::from_mode(mode))
                                .with_context(|| {
                                    format!("failed to chmod {}", dst_path.display())
                                })?;
                        }
                    }
                }
                Err(_) if link_target.is_relative() => {
                    // Dangling relative symlink — the target likely exists in the
                    // base rootfs already (e.g. busybox applet links). Safe to skip
                    // since the overlay only adds files; it doesn't need to duplicate
                    // links whose targets are already present in the base image.
                    continue;
                }
                Err(_) => {
                    // Dangling absolute symlink (rebased under staging root but still
                    // unresolvable) — skip it; the base rootfs should provide the target.
                    continue;
                }
            }
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&src_path)
                    .with_context(|| format!("failed to stat {}", src_path.display()))?
                    .permissions()
                    .mode();
                fs::set_permissions(&dst_path, fs::Permissions::from_mode(mode))
                    .with_context(|| format!("failed to chmod {}", dst_path.display()))?;
            }
        }
    }
    Ok(())
}
