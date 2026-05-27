use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail, ensure};
use ostool::build::config::{Cargo, LogLevel};

use super::build;
use crate::{context::ResolvedBuildRequest, support::process::ProcessExt};

const AX_LIBC_PACKAGE: &str = "ax-libc";

#[derive(Debug, Clone)]
pub(crate) struct ArceosCBuildInput {
    pub(crate) app_dir: PathBuf,
    pub(crate) app_name: String,
    pub(crate) target_dir: PathBuf,
    pub(crate) out_dir: PathBuf,
    pub(crate) features: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArceosCBuildOutput {
    pub(crate) elf_path: PathBuf,
}

pub(crate) fn build_c_app(
    workspace_root: &Path,
    request: &ResolvedBuildRequest,
    input: &ArceosCBuildInput,
) -> anyhow::Result<ArceosCBuildOutput> {
    let mut cargo = build::load_cargo_config(request)?;
    cargo.package = AX_LIBC_PACKAGE.to_string();
    cargo.target = request.target.clone();
    cargo.to_bin = false;
    cargo.features = map_c_app_features(&input.features, &cargo.features);

    let mode = if request.debug { "debug" } else { "release" };
    let arch = request.arch.as_str();
    let arceos_dir = workspace_root.join("os/arceos");
    let axlibc_dir = arceos_dir.join("ulib/axlibc");
    let c_source_dir = axlibc_dir.join("c");
    let include_dir = axlibc_dir.join("include");
    let obj_root = input
        .target_dir
        .join("arceos-c")
        .join(sanitize_name(&input.app_name))
        .join(arch);
    let axlibc_obj_dir = obj_root.join("axlibc");
    let app_obj_dir = obj_root.join("app");
    fs::create_dir_all(&axlibc_obj_dir)
        .with_context(|| format!("failed to create {}", axlibc_obj_dir.display()))?;
    fs::create_dir_all(&app_obj_dir)
        .with_context(|| format!("failed to create {}", app_obj_dir.display()))?;
    fs::create_dir_all(&input.out_dir)
        .with_context(|| format!("failed to create {}", input.out_dir.display()))?;

    build_axlibc_staticlib(workspace_root, &cargo, &input.target_dir, request.debug)?;
    let rust_lib = input
        .target_dir
        .join(&request.target)
        .join(mode)
        .join("libax_libc.a");
    ensure!(
        rust_lib.is_file(),
        "expected ax-libc static library at {}",
        rust_lib.display()
    );

    let linker_script = input
        .target_dir
        .join(&request.target)
        .join(mode)
        .join("linker.x");
    ensure!(
        linker_script.is_file(),
        "expected linker script at {} after ax-libc cargo build",
        linker_script.display()
    );

    let cflags = cflags(
        workspace_root,
        arch,
        mode,
        &include_dir,
        &input.features,
        cargo.log,
    );
    let lib_objects =
        compile_dir_c_sources(&c_source_dir, &axlibc_obj_dir, &cflags, None, "axlibc")?;
    let app_objects = compile_dir_c_sources(&input.app_dir, &app_obj_dir, &cflags, None, "app")?;
    let libc = axlibc_obj_dir.join("libc.a");
    archive_static_lib(arch, &libc, &lib_objects)?;

    let elf_path = input.out_dir.join(format!(
        "{}_{}.unstripped",
        input.app_name,
        platform_name(&cargo.env)
    ));
    link_c_app(
        arch,
        &linker_script,
        &elf_path,
        &rust_lib,
        &libc,
        &app_objects,
        libgcc(arch, &input.features)?,
    )?;

    Ok(ArceosCBuildOutput { elf_path })
}

fn build_axlibc_staticlib(
    workspace_root: &Path,
    cargo: &Cargo,
    target_dir: &Path,
    debug: bool,
) -> anyhow::Result<()> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace_root)
        .arg("build")
        .arg("-p")
        .arg(&cargo.package)
        .arg("--target")
        .arg(&cargo.target)
        .arg("-Z")
        .arg("unstable-options")
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--features")
        .arg(cargo.features.join(","));
    if !debug {
        command.arg("--release");
    }
    for arg in &cargo.args {
        command.arg(arg);
    }
    for (key, value) in &cargo.env {
        command.env(key, value);
    }
    command
        .exec()
        .context("failed to build ax-libc static library")
}

fn cflags(
    workspace_root: &Path,
    arch: &str,
    mode: &str,
    include_dir: &Path,
    features: &[String],
    log: Option<LogLevel>,
) -> Vec<String> {
    let mut flags = vec![
        "-nostdinc".to_string(),
        "-fno-builtin".to_string(),
        "-ffreestanding".to_string(),
        "-Wall".to_string(),
        format!("-I{}", include_dir.display()),
    ];
    for feature in c_config_features(features) {
        flags.push(format!("-DAX_CONFIG_{}", c_define_name(&feature)));
    }
    flags.push(format!(
        "-DAX_LOG_{}",
        format!("{:?}", log.unwrap_or(LogLevel::Warn)).to_uppercase()
    ));
    if mode == "release" {
        flags.push("-O3".to_string());
    }
    match arch {
        "riscv64" => flags.extend([
            "-march=rv64gc".to_string(),
            "-mabi=lp64d".to_string(),
            "-mcmodel=medany".to_string(),
        ]),
        "loongarch64" => flags.push("-msoft-float".to_string()),
        "x86_64" if !has_feature(features, "fp-simd") => flags.push("-mno-sse".to_string()),
        "aarch64" if !has_feature(features, "fp-simd") => {
            flags.push("-mgeneral-regs-only".to_string())
        }
        _ => {}
    }
    flags.push(format!("-I{}", workspace_root.join("include").display()));
    flags
}

fn c_config_features(features: &[String]) -> BTreeSet<String> {
    features
        .iter()
        .filter_map(|feature| {
            if feature.starts_with("ax-hal/") || feature.starts_with("ax-driver/") {
                return None;
            }
            feature
                .strip_prefix("ax-libc/")
                .or_else(|| feature.strip_prefix("ax-feat/"))
                .or_else(|| feature.strip_prefix("ax-std/"))
                .or(Some(feature.as_str()))
        })
        .filter(|feature| {
            !matches!(
                *feature,
                "ax-libc" | "ax-feat" | "ax-std" | "defplat" | "myplat" | "plat-dyn"
            ) && !feature.contains('/')
        })
        .map(str::to_string)
        .collect()
}

fn has_feature(features: &[String], name: &str) -> bool {
    features.iter().any(|feature| {
        feature == name
            || feature.strip_prefix("ax-libc/") == Some(name)
            || feature.strip_prefix("ax-feat/") == Some(name)
            || feature.strip_prefix("ax-std/") == Some(name)
    })
}

fn c_define_name(feature: &str) -> String {
    feature.replace('-', "_").to_uppercase()
}

fn compile_dir_c_sources(
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

fn cc_for_arch(arch: &str) -> String {
    format!("{arch}-linux-musl-gcc")
}

fn map_c_app_features(case_features: &[String], base_features: &[String]) -> Vec<String> {
    const LIB_FEATURES: &[&str] = &[
        "fp-simd",
        "irq",
        "alloc",
        "multitask",
        "lockdep",
        "fs",
        "net",
        "fd",
        "pipe",
        "select",
        "epoll",
    ];

    let mut features = BTreeSet::new();
    for feature in base_features {
        let normalized = feature
            .strip_prefix("ax-feat/")
            .or_else(|| feature.strip_prefix("ax-std/"))
            .or_else(|| feature.strip_prefix("ax-libc/"))
            .unwrap_or(feature);
        if feature.starts_with("ax-hal/") || feature.starts_with("ax-driver/") {
            features.insert(feature.clone());
            continue;
        }
        match normalized {
            "ax-std" | "ax-feat" | "ax-libc" => {}
            "defplat" | "myplat" | "plat-dyn" => {
                features.insert(format!("ax-feat/{normalized}"));
                features.insert(format!("ax-libc/{normalized}"));
            }
            "smp" => {
                features.insert("ax-libc/smp".to_string());
            }
            feature if LIB_FEATURES.contains(&feature) => {
                features.insert(format!("ax-libc/{feature}"));
            }
            feature => {
                features.insert(format!("ax-feat/{feature}"));
            }
        }
    }
    for feature in case_features {
        let normalized = feature
            .strip_prefix("ax-feat/")
            .or_else(|| feature.strip_prefix("ax-std/"))
            .or_else(|| feature.strip_prefix("ax-libc/"))
            .unwrap_or(feature);
        if feature.starts_with("ax-hal/") || feature.starts_with("ax-driver/") {
            features.insert(feature.clone());
            continue;
        }
        if LIB_FEATURES.contains(&normalized) {
            features.insert(format!("ax-libc/{normalized}"));
        } else {
            features.insert(format!("ax-feat/{normalized}"));
        }
    }
    if features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "ax-libc/fs" | "ax-libc/net" | "ax-libc/pipe" | "ax-libc/select" | "ax-libc/epoll"
        )
    }) {
        features.insert("ax-libc/fd".to_string());
    }
    features.into_iter().collect()
}

fn archive_static_lib(arch: &str, path: &Path, objects: &[PathBuf]) -> anyhow::Result<()> {
    let mut command = Command::new(ar_for_arch(arch));
    command.arg("rcs").arg(path).args(objects);
    command
        .exec()
        .with_context(|| format!("failed to archive {}", path.display()))
}

fn ar_for_arch(arch: &str) -> String {
    format!("{arch}-linux-musl-ar")
}

fn link_c_app(
    arch: &str,
    linker_script: &Path,
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
        .arg("-no-pie")
        .arg("--gc-sections")
        .arg("-znostart-stop-gc")
        .arg(format!("-T{}", linker_script.display()));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn c_config_features_skips_nested_cargo_only_features() {
        let features = c_config_features(&strings(&[
            "ax-libc/net",
            "ax-feat/paging",
            "ax-driver/plat-static",
            "ax-driver/virtio-net",
            "ax-hal/riscv64-qemu-virt",
            "some-crate/feature",
        ]));

        assert_eq!(
            features.into_iter().collect::<Vec<_>>(),
            vec!["net".to_string(), "paging".to_string()]
        );
    }

    #[test]
    fn map_c_app_features_preserves_driver_features() {
        let features = map_c_app_features(
            &strings(&["net", "ax-driver/plat-static", "ax-driver/virtio-net"]),
            &strings(&["ax-hal/riscv64-qemu-virt"]),
        );

        assert!(features.contains(&"ax-libc/net".to_string()));
        assert!(features.contains(&"ax-libc/fd".to_string()));
        assert!(features.contains(&"ax-driver/plat-static".to_string()));
        assert!(features.contains(&"ax-driver/virtio-net".to_string()));
        assert!(features.contains(&"ax-hal/riscv64-qemu-virt".to_string()));
    }
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

fn libgcc(arch: &str, features: &[String]) -> anyhow::Result<Option<PathBuf>> {
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

fn platform_name(env: &std::collections::HashMap<String, String>) -> String {
    env.get("AX_PLATFORM")
        .cloned()
        .unwrap_or_else(|| "qemu".to_string())
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}
