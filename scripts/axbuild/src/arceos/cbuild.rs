use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::UNIX_EPOCH,
};

use anyhow::{Context, bail, ensure};
use ostool::build::config::{Cargo, LogLevel};

use super::build;
use crate::{
    build::ARCEOS_LINKER_SCRIPT, context::ResolvedBuildRequest, support::process::ProcessExt,
};

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
    let generated_include_dir = obj_root.join("include");
    let axlibc_obj_dir = obj_root.join("axlibc");
    let app_obj_dir = obj_root.join("app");
    fs::create_dir_all(&axlibc_obj_dir)
        .with_context(|| format!("failed to create {}", axlibc_obj_dir.display()))?;
    fs::create_dir_all(&app_obj_dir)
        .with_context(|| format!("failed to create {}", app_obj_dir.display()))?;
    fs::create_dir_all(&input.out_dir)
        .with_context(|| format!("failed to create {}", input.out_dir.display()))?;

    build_axlibc_staticlib(workspace_root, &cargo, &input.target_dir, request.debug)?;
    write_pthread_mutex_header(&generated_include_dir, &cargo.features)?;
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

    let linker_script = find_final_linker_script(&input.target_dir, &request.target, mode)?;
    let linker_search_dirs = find_linker_search_dirs(
        &input.target_dir,
        &request.target,
        mode,
        platform_name(&cargo.env).as_str(),
        &cargo.features,
    )?;

    let cflags = cflags(
        workspace_root,
        arch,
        mode,
        &generated_include_dir,
        &include_dir,
        &cargo.features,
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
    link_c_app(LinkCAppInput {
        arch,
        linker_script: &linker_script,
        linker_search_dirs: &linker_search_dirs,
        elf_path: &elf_path,
        rust_lib: &rust_lib,
        libc: &libc,
        app_objects: &app_objects,
        libgcc: libgcc(arch, &input.features)?,
    })?;

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

fn find_final_linker_script(
    target_dir: &Path,
    target: &str,
    mode: &str,
) -> anyhow::Result<PathBuf> {
    let build_dir = target_dir.join(target).join(mode).join("build");
    let mut candidates = Vec::new();
    if build_dir.is_dir() {
        for entry in fs::read_dir(&build_dir)
            .with_context(|| format!("failed to read {}", build_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ax-runtime-"))
            {
                let linker_script = path.join("out").join(ARCEOS_LINKER_SCRIPT);
                if linker_script.is_file() {
                    let modified = linker_script
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .unwrap_or(UNIX_EPOCH);
                    candidates.push((modified, linker_script));
                }
            }
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
            format!(
                "expected final linker script under {} after ax-libc cargo build",
                build_dir.join("ax-runtime-*/out").display()
            )
        })
}

fn find_linker_search_dirs(
    target_dir: &Path,
    target: &str,
    mode: &str,
    platform: &str,
    features: &[String],
) -> anyhow::Result<Vec<PathBuf>> {
    let build_dir = target_dir.join(target).join(mode).join("build");
    let mut dirs = BTreeSet::new();
    let runtime_out = latest_out_dir_with_script(&build_dir, "ax-runtime-", ARCEOS_LINKER_SCRIPT)?;
    dirs.insert(runtime_out);
    let platform_out = latest_out_dir_with_script(
        &build_dir,
        platform_linker_owner_prefix(platform, features),
        "axplat.x",
    )?;
    dirs.insert(platform_out);

    Ok(dirs.into_iter().collect())
}

fn latest_out_dir_with_script(
    build_dir: &Path,
    package_prefix: &str,
    script_name: &str,
) -> anyhow::Result<PathBuf> {
    let mut candidates = Vec::new();
    if build_dir.is_dir() {
        for entry in fs::read_dir(build_dir)
            .with_context(|| format!("failed to read {}", build_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(package_prefix))
            {
                continue;
            }
            let out_dir = path.join("out");
            let script = out_dir.join(script_name);
            if script.is_file() {
                let modified = script
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(UNIX_EPOCH);
                candidates.push((modified, out_dir));
            }
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
            format!(
                "expected linker script `{script_name}` under {}/{}*/out after ax-libc cargo build",
                build_dir.display(),
                package_prefix
            )
        })
}

fn platform_linker_owner_prefix(platform: &str, features: &[String]) -> &'static str {
    if has_feature(features, "plat-dyn") {
        return "axplat-dyn-";
    }

    match platform {
        "loongarch64-qemu-virt" => "ax-plat-loongarch64-qemu-virt-",
        "x86-qemu-q35" => "ax-plat-x86-qemu-q35-",
        _ => "ax-hal-",
    }
}

fn cflags(
    workspace_root: &Path,
    arch: &str,
    mode: &str,
    generated_include_dir: &Path,
    include_dir: &Path,
    features: &[String],
    log: Option<LogLevel>,
) -> Vec<String> {
    let mut flags = vec![
        "-nostdinc".to_string(),
        "-fno-builtin".to_string(),
        "-ffreestanding".to_string(),
        "-Wall".to_string(),
        format!("-I{}", generated_include_dir.display()),
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

fn write_pthread_mutex_header(include_dir: &Path, features: &[String]) -> anyhow::Result<()> {
    fs::create_dir_all(include_dir)
        .with_context(|| format!("failed to create {}", include_dir.display()))?;
    let path = include_dir.join("ax_pthread_mutex.h");
    fs::write(&path, pthread_mutex_header_contents(features))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn pthread_mutex_header_contents(features: &[String]) -> String {
    let (mutex_size, mutex_init) = pthread_mutex_layout(features);
    format!(
        r#"// Generated by axbuild for this C app build - DO NOT edit!

typedef struct {{
    long __l[{mutex_size}];
}} pthread_mutex_t;

#define PTHREAD_MUTEX_INITIALIZER {{ .__l = {mutex_init} }}
"#
    )
}

fn pthread_mutex_layout(features: &[String]) -> (usize, &'static str) {
    if !has_feature(features, "multitask") {
        return (1, "{0}");
    }

    if has_feature(features, "lockdep") {
        if has_feature(features, "smp") {
            return (10, "{-1, 0, 0, 0, 0, 0, 0, 0, 0, 0}");
        }
        return (9, "{-1, 0, 0, 0, 0, 0, 0, 0, 0}");
    }

    if has_feature(features, "smp") {
        (6, "{0, 0, 8, 0, 0, 0}")
    } else {
        (5, "{0, 8, 0, 0, 0}")
    }
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

struct LinkCAppInput<'a> {
    arch: &'a str,
    linker_script: &'a Path,
    linker_search_dirs: &'a [PathBuf],
    elf_path: &'a Path,
    rust_lib: &'a Path,
    libc: &'a Path,
    app_objects: &'a [PathBuf],
    libgcc: Option<PathBuf>,
}

fn link_c_app(input: LinkCAppInput<'_>) -> anyhow::Result<()> {
    let mut command = Command::new("rust-lld");
    command
        .arg("-flavor")
        .arg("gnu")
        .arg("-m")
        .arg(lld_machine(input.arch)?)
        .arg("-nostdlib")
        .arg("-static")
        .arg("-no-pie")
        .arg("--gc-sections")
        .arg("-znostart-stop-gc")
        .arg(format!("-T{}", input.linker_script.display()));
    for dir in input.linker_search_dirs {
        command.arg(format!("-L{}", dir.display()));
    }
    if let Some(libgcc) = input.libgcc {
        command.arg(libgcc);
    }
    command
        .args(input.app_objects)
        .arg(input.libc)
        .arg(input.rust_lib)
        .arg("-o")
        .arg(input.elf_path);
    command
        .exec()
        .with_context(|| format!("failed to link {}", input.elf_path.display()))
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

    #[test]
    fn pthread_mutex_header_matches_lockdep_smp_layout() {
        let header = pthread_mutex_header_contents(&strings(&[
            "ax-libc/multitask",
            "ax-libc/lockdep",
            "ax-libc/smp",
        ]));

        assert!(header.contains("long __l[10];"));
        assert!(header.contains("{-1, 0, 0, 0, 0, 0, 0, 0, 0, 0}"));
    }

    #[test]
    fn pthread_mutex_header_matches_plain_smp_layout() {
        let header = pthread_mutex_header_contents(&strings(&["ax-libc/multitask", "ax-libc/smp"]));

        assert!(header.contains("long __l[6];"));
        assert!(header.contains("{0, 0, 8, 0, 0, 0}"));
    }

    #[test]
    fn final_linker_script_comes_from_axruntime_build_out_dir() {
        let root = tempfile::tempdir().unwrap();
        let target_dir = root.path().join("target");
        let target = "x86_64-unknown-none";
        let mode = "release";
        let stable_dir = target_dir.join(target).join(mode);
        let out_dir = stable_dir.join("build/ax-runtime-abc/out");
        fs::create_dir_all(&out_dir).unwrap();
        fs::create_dir_all(&stable_dir).unwrap();
        fs::write(stable_dir.join(ARCEOS_LINKER_SCRIPT), "stable").unwrap();
        fs::write(out_dir.join(ARCEOS_LINKER_SCRIPT), "runtime").unwrap();

        let linker = find_final_linker_script(&target_dir, target, mode).unwrap();

        assert_eq!(linker, out_dir.join(ARCEOS_LINKER_SCRIPT));
    }

    #[test]
    fn linker_search_dirs_use_current_platform_script_owner() {
        let root = tempfile::tempdir().unwrap();
        let target_dir = root.path().join("target");
        let target = "x86_64-unknown-none";
        let mode = "release";
        let build_dir = target_dir.join(target).join(mode).join("build");
        let axhal_out = build_dir.join("ax-hal-abc/out");
        let q35_out = build_dir.join("ax-plat-x86-qemu-q35-abc/out");
        let stale_loong_out = build_dir.join("ax-plat-loongarch64-qemu-virt-abc/out");
        let runtime_out = build_dir.join("ax-runtime-def/out");
        let unrelated_out = build_dir.join("unrelated-ghi/out");
        fs::create_dir_all(&axhal_out).unwrap();
        fs::create_dir_all(&q35_out).unwrap();
        fs::create_dir_all(&stale_loong_out).unwrap();
        fs::create_dir_all(&runtime_out).unwrap();
        fs::create_dir_all(&unrelated_out).unwrap();
        fs::write(axhal_out.join("axplat.x"), "").unwrap();
        fs::write(q35_out.join("axplat.x"), "").unwrap();
        fs::write(stale_loong_out.join("axplat.x"), "").unwrap();
        fs::write(runtime_out.join(ARCEOS_LINKER_SCRIPT), "").unwrap();
        fs::write(unrelated_out.join("note.txt"), "").unwrap();

        let dirs = find_linker_search_dirs(
            &target_dir,
            target,
            mode,
            "x86-qemu-q35",
            &strings(&["ax-hal/x86-qemu-q35"]),
        )
        .unwrap();

        assert_eq!(dirs, vec![q35_out, runtime_out]);
    }

    #[test]
    fn linker_search_dirs_use_axhal_for_generic_static_platforms() {
        let root = tempfile::tempdir().unwrap();
        let target_dir = root.path().join("target");
        let target = "aarch64-unknown-none-softfloat";
        let mode = "release";
        let build_dir = target_dir.join(target).join(mode).join("build");
        let axhal_out = build_dir.join("ax-hal-abc/out");
        let runtime_out = build_dir.join("ax-runtime-def/out");
        fs::create_dir_all(&axhal_out).unwrap();
        fs::create_dir_all(&runtime_out).unwrap();
        fs::write(axhal_out.join("axplat.x"), "").unwrap();
        fs::write(runtime_out.join(ARCEOS_LINKER_SCRIPT), "").unwrap();

        let dirs = find_linker_search_dirs(
            &target_dir,
            target,
            mode,
            "aarch64-qemu-virt",
            &strings(&["ax-hal/aarch64-qemu-virt"]),
        )
        .unwrap();

        assert_eq!(dirs, vec![axhal_out, runtime_out]);
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
