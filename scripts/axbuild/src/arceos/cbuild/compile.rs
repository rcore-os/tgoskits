use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, ensure};

use crate::support::process::ProcessExt;

pub(super) fn compile_dir_c_sources(
    source_dir: &Path,
    obj_dir: &Path,
    cflags: &[String],
    extra_cflags: Option<&[String]>,
    label: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut objects = Vec::new();
    let mut sources = fs::read_dir(source_dir)
        .with_context(|| format!("failed to read {}", source_dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    sources.sort_by_key(|entry| entry.path());
    for entry in sources {
        let source = entry.path();
        if source.extension().is_none_or(|ext| ext != "c") {
            continue;
        }
        let object = obj_dir.join(format!(
            "{}.o",
            source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .context("invalid C source filename")?
        ));
        compile_c_source(&source, &object, cflags, extra_cflags)
            .with_context(|| format!("failed to compile {label} source {}", source.display()))?;
        objects.push(object);
    }
    Ok(objects)
}

fn compile_c_source(
    source: &Path,
    object: &Path,
    cflags: &[String],
    extra_cflags: Option<&[String]>,
) -> anyhow::Result<()> {
    ensure!(source.is_file(), "missing C source {}", source.display());
    let mut command = Command::new(cc_for_arch(source_arch_hint(cflags)));
    command.args(cflags);
    if let Some(extra_cflags) = extra_cflags {
        command.args(extra_cflags);
    }
    command.arg("-c").arg("-o").arg(object).arg(source);
    command.exec()
}

fn source_arch_hint(cflags: &[String]) -> &str {
    if cflags.iter().any(|flag| flag == "-march=rv64gc") {
        "riscv64"
    } else if cflags.iter().any(|flag| flag == "-msoft-float") {
        "loongarch64"
    } else if cflags.iter().any(|flag| flag == "-mgeneral-regs-only") {
        "aarch64"
    } else {
        "x86_64"
    }
}

pub(super) fn cc_for_arch(arch: &str) -> String {
    format!("{arch}-linux-musl-gcc")
}

pub(super) fn archive_static_lib(
    arch: &str,
    path: &Path,
    objects: &[PathBuf],
) -> anyhow::Result<()> {
    let mut command = Command::new(ar_for_arch(arch));
    command.arg("rcs").arg(path).args(objects);
    command
        .exec()
        .with_context(|| format!("failed to archive {}", path.display()))
}

fn ar_for_arch(arch: &str) -> String {
    format!("{arch}-linux-musl-ar")
}
