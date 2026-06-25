use super::*;

pub(super) fn apply_case_script_envs(
    command: &mut Command,
    layout: &case_assets::CaseAssetLayout,
    script_envs: &[(String, String)],
) -> anyhow::Result<()> {
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = Vec::new();
    path_entries.push(layout.command_wrapper_dir.clone());
    path_entries.extend(std::env::split_paths(&host_path));

    let path = std::env::join_paths(path_entries)
        .map_err(|e| anyhow::anyhow!("failed to build case script PATH: {e}"))?;
    command.env("PATH", path);
    command.env("QEMU_LD_PREFIX", &layout.staging_root);
    command.env("LD_LIBRARY_PATH", guest_library_path(&layout.staging_root));

    for (key, value) in script_envs {
        command.env(key, value);
    }

    Ok(())
}

pub(crate) fn case_script_envs(
    case: &TestQemuCase,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> Vec<(String, String)> {
    vec![
        (
            config.script_env.staging_root.clone(),
            layout.staging_root.display().to_string(),
        ),
        (
            config.script_env.case_dir.clone(),
            case.case_dir.display().to_string(),
        ),
        (
            config.script_env.case_c_dir.clone(),
            case_c_source_dir(case).display().to_string(),
        ),
        (
            config.script_env.case_work_dir.clone(),
            layout.work_dir.display().to_string(),
        ),
        (
            config.script_env.case_build_dir.clone(),
            layout.build_dir.display().to_string(),
        ),
        (
            config.script_env.case_overlay_dir.clone(),
            layout.overlay_dir.display().to_string(),
        ),
    ]
}

pub(super) fn write_guest_command_wrappers(
    layout: &case_assets::CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    let mut guest_commands = BTreeMap::new();
    for relative_dir in ["bin", "sbin", "usr/bin", "usr/sbin"] {
        let dir_path = layout.staging_root.join(relative_dir);
        if !dir_path.is_dir() {
            continue;
        }

        let mut entries = fs::read_dir(&dir_path)
            .with_context(|| format!("failed to read {}", dir_path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read {}", dir_path.display()))?;
        entries.sort_by_key(|left| left.file_name());

        for entry in entries {
            let name = entry.file_name();
            if guest_commands.contains_key(name.as_os_str()) {
                continue;
            }

            let file_type = entry.file_type().with_context(|| {
                format!("failed to inspect guest command {}", entry.path().display())
            })?;
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            guest_commands.insert(
                name,
                format!("{relative_dir}/{}", entry.file_name().to_string_lossy()),
            );
        }
    }

    for (name, relative_guest_path) in guest_commands {
        let wrapper_path = layout.command_wrapper_dir.join(&name);
        if relative_guest_path == "sbin/apk" {
            write_apk_wrapper_script(&wrapper_path, qemu_runner, &layout.staging_root, layout)?;
        } else {
            write_guest_exec_wrapper(
                &wrapper_path,
                qemu_runner,
                &layout.staging_root,
                &relative_guest_path,
                None,
            )?;
        }
    }

    Ok(())
}

pub(super) fn ensure_guest_tool_exists(
    staging_root: &Path,
    relative_path: &str,
) -> anyhow::Result<()> {
    let path = staging_root.join(relative_path);
    ensure!(
        path.is_file(),
        "staging root is missing required guest tool `{}`; install it in prebuild.sh",
        path.display()
    );
    Ok(())
}

pub(crate) fn qemu_user_binary_names(arch: &str) -> anyhow::Result<&'static [&'static str]> {
    Ok(cross_compile_spec(arch)?.qemu_user_binaries)
}

pub(super) fn write_guest_exec_wrapper(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    guest_relative_path: &str,
    extra_args: Option<String>,
) -> anyhow::Result<()> {
    let guest_path = staging_root.join(guest_relative_path);
    let mut body = format!(
        "export QEMU_LD_PREFIX={root}\nexport LD_LIBRARY_PATH={lib_path}\nexec {qemu} -0 {guest} \
         -L {root} {guest}",
        root = shell_single_quote(staging_root),
        lib_path = shell_single_quote(guest_library_path(staging_root)),
        qemu = shell_single_quote(qemu_runner),
        guest = shell_single_quote(&guest_path),
    );
    if let Some(extra_args) = extra_args {
        body.push(' ');
        body.push_str(&extra_args);
    }
    body.push_str(" \"$@\"\n");

    write_wrapper_script(path, &body)
}

pub(super) fn write_apk_wrapper_script(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    layout: &case_assets::CaseAssetLayout,
) -> anyhow::Result<()> {
    let body = format!(
        "export QEMU_LD_PREFIX={root}\nexport LD_LIBRARY_PATH={lib_path}\nexec {qemu} -L {root} \
         {apk} --root {root} --repositories-file {repositories} --keys-dir {keys} --cache-dir \
         {cache} --update-cache --timeout 60 --no-interactive --force-no-chroot --scripts=no \
         \"$@\"\n",
        root = shell_single_quote(staging_root),
        lib_path = shell_single_quote(guest_library_path(staging_root)),
        qemu = shell_single_quote(qemu_runner),
        apk = shell_single_quote(staging_root.join("sbin/apk")),
        repositories = shell_single_quote(staging_root.join("etc/apk/repositories")),
        keys = shell_single_quote(staging_root.join("etc/apk/keys")),
        cache = shell_single_quote(&layout.apk_cache_dir),
    );
    write_wrapper_script(path, &body)
}

pub(super) fn guest_library_path(staging_root: &Path) -> String {
    format!(
        "{}:{}",
        staging_root.join("lib").display(),
        staging_root.join("usr/lib").display()
    )
}

pub(super) fn write_wrapper_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

pub(super) fn find_host_binary_candidates(candidates: &[&str]) -> anyhow::Result<PathBuf> {
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

pub(super) fn find_optional_host_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

pub(super) fn shell_single_quote(path: impl AsRef<Path>) -> String {
    let value = path.as_ref().display().to_string().replace('\'', "'\\''");
    format!("'{value}'")
}
