use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::UNIX_EPOCH,
};

use anyhow::{Context, bail};

use super::{compile::cc_for_arch, features::has_feature};
use crate::{build::ARCEOS_LINKER_SCRIPT, support::process::ProcessExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LinkScripts {
    pub(super) script: PathBuf,
    pub(super) search_dirs: Vec<PathBuf>,
    pub(super) pie: bool,
}

pub(super) fn find_link_scripts(
    target_dir: &Path,
    target: &str,
    mode: &str,
    platform: &str,
    features: &[String],
) -> anyhow::Result<LinkScripts> {
    let script = find_final_linker_script(target_dir, target, mode)?;
    let search_dirs = find_dynamic_linker_search_dirs(target_dir, target, mode)?;
    let _ = (platform, features);
    Ok(LinkScripts {
        script,
        search_dirs,
        pie: true,
    })
}

pub(super) fn find_final_linker_script(
    target_dir: &Path,
    target: &str,
    mode: &str,
) -> anyhow::Result<PathBuf> {
    let build_dir = target_dir.join(target).join(mode).join("build");
    latest_script_with_package_prefix(&build_dir, "ax-runtime-", ARCEOS_LINKER_SCRIPT)
}

#[cfg(test)]
pub(super) fn find_linker_search_dirs(
    target_dir: &Path,
    target: &str,
    mode: &str,
    _platform: &str,
    features: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    let build_dir = target_dir.join(target).join(mode).join("build");
    let mut dirs = BTreeSet::new();
    let runtime_out = latest_out_dir_with_script(&build_dir, "ax-runtime-", ARCEOS_LINKER_SCRIPT)?;
    dirs.insert(runtime_out);
    let platform_out = latest_out_dir_with_script(&build_dir, "axplat-dyn-", "axplat.x")?;
    let _ = features;
    dirs.insert(platform_out);

    Ok(dirs.into_iter().collect())
}

fn find_dynamic_linker_search_dirs(
    target_dir: &Path,
    target: &str,
    mode: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let build_dir = target_dir.join(target).join(mode).join("build");
    let mut dirs = BTreeSet::new();
    dirs.insert(latest_out_dir_with_script(
        &build_dir,
        "ax-runtime-",
        ARCEOS_LINKER_SCRIPT,
    )?);
    dirs.insert(latest_out_dir_with_script(
        &build_dir,
        "axplat-dyn-",
        "axplat.x",
    )?);
    dirs.insert(latest_out_dir_with_script(
        &build_dir, "somehal-", "link.x",
    )?);
    dirs.insert(latest_out_dir_with_script(
        &build_dir,
        "someboot-",
        "someboot.x",
    )?);
    Ok(dirs.into_iter().collect())
}

fn latest_out_dir_with_script(
    build_dir: &Path,
    package_prefix: &str,
    script_name: &str,
) -> anyhow::Result<PathBuf> {
    let script = latest_script_with_package_prefix(build_dir, package_prefix, script_name)?;
    Ok(script
        .parent()
        .expect("a build script output always has an out directory")
        .to_path_buf())
}

fn latest_script_with_package_prefix(
    build_dir: &Path,
    package_prefix: &str,
    script_name: &str,
) -> anyhow::Result<PathBuf> {
    let mut candidates = Vec::new();
    for out_dir in package_out_dirs(build_dir, package_prefix)? {
        let script = out_dir.join(script_name);
        if script.is_file() {
            let modified = script
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            candidates.push((modified, script));
        }
    }

    candidates.sort_by(|(left_time, left_path), (right_time, right_path)| {
        right_time
            .cmp(left_time)
            .then_with(|| left_path.cmp(right_path))
    });
    candidates
        .into_iter()
        .map(|(_, path)| path)
        .next()
        .with_context(|| {
            let package = package_prefix.trim_end_matches('-');
            format!(
                "expected linker script `{script_name}` under {}/{}*/out or {}/{package}/*/out \
                 after ax-libc cargo build",
                build_dir.display(),
                package_prefix,
                build_dir.display(),
            )
        })
}

fn package_out_dirs(build_dir: &Path, package_prefix: &str) -> anyhow::Result<Vec<PathBuf>> {
    if !build_dir.is_dir() {
        return Ok(Vec::new());
    }

    let package = package_prefix.trim_end_matches('-');
    let mut out_dirs = Vec::new();
    for entry in fs::read_dir(build_dir)
        .with_context(|| format!("failed to read {}", build_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(package_prefix) {
            out_dirs.push(path.join("out"));
        } else if name == package && path.is_dir() {
            for unit in
                fs::read_dir(&path).with_context(|| format!("failed to read {}", path.display()))?
            {
                let unit = unit?;
                if unit.path().is_dir() {
                    out_dirs.push(unit.path().join("out"));
                }
            }
        }
    }
    Ok(out_dirs)
}

pub(super) fn link_c_app(
    arch: &str,
    link_scripts: &LinkScripts,
    elf_path: &Path,
    rust_lib: &Path,
    libc: &Path,
    app_objects: &[PathBuf],
    libgcc: Option<PathBuf>,
) -> anyhow::Result<()> {
    let mut command = Command::new("rust-lld");
    command
        .arg("-flavor")
        .arg("gnu")
        .arg("-m")
        .arg(lld_machine(arch)?)
        .arg("-nostdlib")
        .arg("-static")
        .arg("--gc-sections")
        .arg("-znostart-stop-gc");
    for dir in &link_scripts.search_dirs {
        command.arg(format!("-L{}", dir.display()));
    }
    command.arg(format!("-T{}", link_scripts.script.display()));
    if link_scripts.pie {
        command.arg("-pie");
    } else {
        command.arg("-no-pie");
    }
    if let Some(libgcc) = libgcc {
        command.arg(libgcc);
    }
    command
        .args(app_objects)
        .arg(libc)
        .arg(rust_lib)
        .arg("-o")
        .arg(elf_path);
    command
        .exec()
        .with_context(|| format!("failed to link {}", elf_path.display()))
}

fn lld_machine(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("aarch64elf"),
        "loongarch64" => Ok("elf64loongarch"),
        "riscv64" => Ok("elf64lriscv"),
        "x86_64" => Ok("elf_x86_64"),
        arch => bail!("unsupported ArceOS C link architecture `{arch}`"),
    }
}

pub(super) fn libgcc(arch: &str, features: &[String]) -> anyhow::Result<Option<PathBuf>> {
    if !has_feature(features, "fp-simd") || !matches!(arch, "riscv64" | "aarch64") {
        return Ok(None);
    }
    let output = Command::new(cc_for_arch(arch))
        .arg("-print-libgcc-file-name")
        .stdout(Stdio::piped())
        .output()
        .context("failed to query libgcc path")?;
    if !output.status.success() {
        bail!("failed to query libgcc path with status {}", output.status);
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

pub(super) fn platform_name(env: &HashMap<String, String>) -> String {
    env.get("AX_PLATFORM")
        .cloned()
        .unwrap_or_else(|| "qemu".to_string())
}
