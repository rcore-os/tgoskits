//! `cargo xtask starry kmod build` — build StarryOS loadable kernel modules.
//!
//! The important part of the upstream Makefile flow is not Make itself. It is
//! that modules are compiled with the same target, features, generated
//! axconfig, and Cargo target directory as the kernel. This implementation
//! derives a normal Starry Cargo config first, then switches only the package
//! and output handling for each module crate before partial-linking the module
//! rlib into an ET_REL `.ko`.
//!
//! C modules built with Linux Kbuild are intentionally host-only here. axbuild
//! calls the module directory's own Makefile only when the selected Starry
//! architecture matches the host architecture.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::{Metadata, Package, TargetKind};
use clap::{Args, Subcommand};
use ostool::build::config::{Cargo, CargoBuildProfile};

use crate::{
    context::SnapshotPersistence,
    starry::{ArgsBuild, Starry, build},
};

/// Top-level CLI args for `cargo xtask starry kmod ...`.
#[derive(Args)]
pub struct ArgsKmod {
    #[command(subcommand)]
    pub command: KmodCommand,
}

/// `kmod` subcommands.
#[derive(Subcommand)]
pub enum KmodCommand {
    /// Build one or more module crates into `.ko` images.
    Build(ArgsKmodBuild),
}

#[derive(Args)]
pub struct ArgsKmodBuild {
    #[command(flatten)]
    pub build: ArgsBuild,

    /// Path to a module crate (or directory containing module crates,
    /// auto-discovered via `Cargo.toml` lookup, depth ≤ 10). May be
    /// repeated.
    #[arg(short = 'm', long, value_name = "PATH")]
    pub module: Vec<PathBuf>,

    /// Build every module crate under `os/StarryOS/lkm/`.
    #[arg(long, conflicts_with = "module")]
    pub all: bool,

    /// Inject built modules into this rootfs image under `/modules/`.
    #[arg(long, value_name = "IMAGE")]
    pub rootfs: Option<PathBuf>,
}

impl Starry {
    /// Entry point for `cargo xtask starry kmod ...`.
    pub async fn kmod(&mut self, args: ArgsKmod) -> Result<()> {
        match args.command {
            KmodCommand::Build(b) => self.kmod_build(b).await,
        }
    }

    async fn kmod_build(&mut self, args: ArgsKmodBuild) -> Result<()> {
        let workspace_root = self.app.workspace_root().to_path_buf();
        let modules = collect_modules(&workspace_root, &args)?;
        if modules.is_empty() {
            bail!("no module crates found (use `--module <path>` or `--all`)");
        }

        let request =
            self.prepare_request((&args.build).into(), None, None, SnapshotPersistence::Store)?;
        self.ensure_default_build_config_for_request(&request, "kmod")?;
        self.app.set_debug_mode(request.debug)?;
        let base_cargo = build::load_cargo_config(&request)?;
        let metadata = crate::build::cached_workspace_metadata()
            .context("failed to load workspace metadata")?;

        let linker_script = workspace_root.join("os/StarryOS/scripts/kmod-linker.ld");
        if !linker_script.exists() {
            bail!(
                "kmod linker script not found at {}",
                linker_script.display()
            );
        }

        let profile = cargo_profile_dir(&base_cargo, request.debug);
        let target_dir = cargo_target_output_dir(&workspace_root, &base_cargo.target, profile)?;
        std::fs::create_dir_all(&target_dir)
            .with_context(|| format!("create {}", target_dir.display()))?;

        let mut built_modules = Vec::new();
        for module in modules {
            match module {
                ModuleSpec::Rust(module_path) => {
                    let ko_path = build_one_rust_module(
                        &workspace_root,
                        &module_path,
                        &base_cargo,
                        request.debug,
                        metadata,
                        &linker_script,
                        &target_dir,
                    )?;
                    built_modules.push(ko_path);
                }
                ModuleSpec::LinuxC(module_path) => {
                    if let Some(ko_paths) =
                        build_one_linux_c_module(&module_path, &request.arch, &target_dir)?
                    {
                        built_modules.extend(ko_paths);
                    }
                }
            }
        }

        if let Some(rootfs) = args.rootfs {
            inject_modules_into_rootfs(&workspace_root, &rootfs, &built_modules)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ModuleSpec {
    Rust(PathBuf),
    LinuxC(PathBuf),
}

fn collect_modules(workspace_root: &Path, args: &ArgsKmodBuild) -> Result<Vec<ModuleSpec>> {
    if args.all || args.module.is_empty() {
        let modules_dir = workspace_root.join("os/StarryOS/lkm");
        if !modules_dir.exists() {
            return Ok(Vec::new());
        }
        return discover_modules(&modules_dir);
    }

    let mut paths = Vec::new();
    for raw in &args.module {
        let resolved = if raw.is_absolute() {
            raw.clone()
        } else {
            workspace_root.join(raw)
        };
        if resolved.join("Cargo.toml").exists() {
            paths.push(ModuleSpec::Rust(resolved));
        } else if is_linux_c_module_dir(&resolved)? {
            paths.push(ModuleSpec::LinuxC(resolved));
        } else {
            paths.extend(discover_modules(&resolved)?);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn discover_modules(root: &Path) -> Result<Vec<ModuleSpec>> {
    let mut out = Vec::new();
    discover_modules_inner(root, 0, &mut out)?;
    out.sort();
    out.dedup();
    Ok(out)
}

fn discover_modules_inner(dir: &Path, depth: usize, out: &mut Vec<ModuleSpec>) -> Result<()> {
    if depth > 10 {
        return Ok(());
    }
    if dir.join("Cargo.toml").exists() {
        out.push(ModuleSpec::Rust(dir.to_path_buf()));
        return Ok(());
    }
    if is_linux_c_module_dir(dir)? {
        out.push(ModuleSpec::LinuxC(dir.to_path_buf()));
        return Ok(());
    }
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            discover_modules_inner(&path, depth + 1, out)?;
        }
    }
    Ok(())
}

fn is_linux_c_module_dir(dir: &Path) -> Result<bool> {
    let makefile = dir.join("Makefile");
    if !makefile.exists() {
        return Ok(false);
    }
    let content = fs::read_to_string(&makefile)
        .with_context(|| format!("failed to read {}", makefile.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .any(|line| !line.starts_with('#') && line.starts_with("obj-m")))
}

fn build_one_rust_module(
    workspace_root: &Path,
    module_path: &Path,
    base_cargo: &Cargo,
    debug: bool,
    metadata: &Metadata,
    linker_script: &Path,
    out_dir: &Path,
) -> Result<PathBuf> {
    let cargo_toml = module_path.join("Cargo.toml");
    if !cargo_toml.exists() {
        bail!("missing {}", cargo_toml.display());
    }
    let package = package_for_manifest(metadata, &cargo_toml)?;
    let lib_name = lib_target_name(package)?;
    let module_name = package.name.to_string();

    println!("[kmod] building {module_name} for {}", base_cargo.target);

    let module_cargo = module_cargo_config(base_cargo, package, debug);
    cargo_build_module_rlib(workspace_root, &module_cargo, debug)?;

    let profile = cargo_profile_dir(&module_cargo, debug);
    let rlib_path = cargo_target_output_dir(workspace_root, &module_cargo.target, profile)?
        .join(format!("lib{}.rlib", rust_crate_file_stem(lib_name)));
    if !rlib_path.exists() {
        bail!("expected module rlib not found at {}", rlib_path.display());
    }

    // Step 3: partial-link into a .ko via the kmod linker script.
    let ko_path = out_dir.join(format!("{module_name}.ko"));
    // `(program, leading_args)`: the default ships `-flavor gnu` so `rust-lld`
    // runs as the GNU ELF driver; a `KMOD_LINKER` override is used verbatim.
    let (linker, lead_args): (String, &[&str]) = match std::env::var("KMOD_LINKER") {
        Ok(l) => (l, &[]),
        Err(_) => {
            let (prog, args) = pick_linker(&module_cargo.target);
            (prog.into(), args)
        }
    };
    let status = Command::new(&linker)
        .args(lead_args)
        .args(["-r", "-T"])
        .arg(linker_script)
        .arg("-o")
        .arg(&ko_path)
        .arg("--whole-archive")
        .arg(&rlib_path)
        .args([
            "--strip-debug",
            "--build-id=none",
            "--gc-sections",
            "-no-pie",
        ])
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("invoke {linker} -r for {module_name}"))?;
    if !status.success() {
        bail!("ld -r failed for {module_name}");
    }

    println!("[kmod] wrote {}", ko_path.display());
    Ok(ko_path)
}

fn build_one_linux_c_module(
    module_path: &Path,
    starry_arch: &str,
    out_dir: &Path,
) -> Result<Option<Vec<PathBuf>>> {
    let host_arch = normalized_host_arch();
    if host_arch != starry_arch {
        println!(
            "[kmod] skipping Linux C module {}: Starry arch `{}` does not match host arch `{}`",
            module_path.display(),
            starry_arch,
            host_arch
        );
        return Ok(None);
    }

    let makefile = module_path.join("Makefile");
    let module_names = linux_c_module_names(&makefile)?;
    if module_names.is_empty() {
        bail!("no obj-m entries found in {}", makefile.display());
    }

    println!("[kmod] building Linux C module {}", module_path.display());
    let status = Command::new("make")
        .args(linux_c_module_make_args(module_path))
        .status()
        .with_context(|| format!("invoke Makefile for {}", module_path.display()))?;
    if !status.success() {
        bail!(
            "Linux C module Makefile failed for {}",
            module_path.display()
        );
    }

    let mut built = Vec::new();
    for module_name in module_names {
        let source_ko = module_path.join(format!("{module_name}.ko"));
        if !source_ko.exists() {
            bail!(
                "expected Linux C module not found at {}",
                source_ko.display()
            );
        }
        let ko_path = out_dir.join(format!("{module_name}.ko"));
        fs::copy(&source_ko, &ko_path).with_context(|| {
            format!(
                "failed to copy {} to {}",
                source_ko.display(),
                ko_path.display()
            )
        })?;
        println!("[kmod] wrote {}", ko_path.display());
        built.push(ko_path);
    }

    clean_linux_c_module(module_path)?;
    Ok(Some(built))
}

fn linux_c_module_make_args(module_path: &Path) -> Vec<String> {
    vec![
        "-C".to_string(),
        module_path.display().to_string(),
        format!("PWD={}", module_path.display()),
    ]
}

fn clean_linux_c_module(module_path: &Path) -> Result<()> {
    println!("[kmod] cleaning Linux C module {}", module_path.display());
    let status = Command::new("make")
        .args(linux_c_module_make_args(module_path))
        .arg("clean")
        .status()
        .with_context(|| format!("invoke Makefile clean for {}", module_path.display()))?;
    if !status.success() {
        bail!(
            "Linux C module Makefile clean failed for {}",
            module_path.display()
        );
    }
    Ok(())
}

fn normalized_host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "riscv64" => "riscv64",
        "loongarch64" => "loongarch64",
        _ => std::env::consts::ARCH,
    }
}

fn linux_c_module_names(makefile: &Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(makefile)
        .with_context(|| format!("failed to read {}", makefile.display()))?;
    let mut names = Vec::new();
    for line in content.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if !line.starts_with("obj-m") {
            continue;
        }
        let Some((_, value)) = line
            .split_once("+=")
            .or_else(|| line.split_once(":="))
            .or_else(|| line.split_once('='))
        else {
            continue;
        };
        for object in value.split_whitespace() {
            let Some(name) = object.strip_suffix(".o") else {
                continue;
            };
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn module_cargo_config(base: &Cargo, package: &Package, debug: bool) -> Cargo {
    let mut cargo = base.clone();
    cargo.package = package.name.to_string();
    cargo.bin = None;
    cargo.to_bin = false;
    cargo.pre_build_cmds.clear();
    cargo.post_build_cmds.clear();
    remove_arg_value(&mut cargo.args, "--bin");
    cargo.features = module_features(&cargo, package, debug);
    cargo
}

fn module_features(cargo: &Cargo, package: &Package, debug: bool) -> Vec<String> {
    let mut features = cargo.features.clone();
    if let Some(log) = &cargo.log
        && package.dependencies.iter().any(|dep| dep.name == "log")
    {
        features.push(format!(
            "log/{}max_level_{}",
            if effective_profile(cargo, debug) == CargoBuildProfile::Debug {
                ""
            } else {
                "release_"
            },
            format!("{log:?}").to_lowercase()
        ));
    }
    features.sort();
    features.dedup();
    features
}

fn remove_arg_value(args: &mut Vec<String>, key: &str) {
    let mut out = Vec::with_capacity(args.len());
    {
        let mut iter = args.drain(..);
        while let Some(arg) = iter.next() {
            if arg == key {
                let _ = iter.next();
                continue;
            }
            out.push(arg);
        }
    }
    *args = out;
}

fn cargo_build_module_rlib(workspace_root: &Path, cargo: &Cargo, debug: bool) -> Result<()> {
    if let Some(extra_config) = &cargo.extra_config
        && (extra_config.starts_with("http://") || extra_config.starts_with("https://"))
    {
        bail!("URL Cargo extra_config is not supported for kmod builds: {extra_config}");
    }

    let mut command = Command::new("cargo");
    command
        .arg("build")
        .arg("-p")
        .arg(&cargo.package)
        .arg("--target")
        .arg(&cargo.target)
        .arg("-Z")
        .arg("unstable-options")
        .arg("--target-dir")
        .arg(workspace_root.join("target"));

    if let Some(extra_config) = &cargo.extra_config {
        command.arg("--config").arg(extra_config);
    }

    if !cargo.features.is_empty() {
        command.arg("--features").arg(cargo.features.join(","));
    }

    command.args(&cargo.args);

    if effective_profile(cargo, debug) == CargoBuildProfile::Release {
        command.arg("--release");
    }

    command.current_dir(workspace_root);
    command.envs(&cargo.env);

    let status = command
        .status()
        .with_context(|| format!("invoke cargo build for {}", cargo.package))?;
    if !status.success() {
        bail!("cargo build failed for {}", cargo.package);
    }
    Ok(())
}

fn effective_profile(cargo: &Cargo, debug: bool) -> CargoBuildProfile {
    cargo.profile.unwrap_or(if debug {
        CargoBuildProfile::Debug
    } else {
        CargoBuildProfile::Release
    })
}

fn cargo_profile_dir(cargo: &Cargo, debug: bool) -> &'static str {
    match effective_profile(cargo, debug) {
        CargoBuildProfile::Debug => "debug",
        CargoBuildProfile::Release => "release",
    }
}

fn cargo_target_output_dir(
    workspace_root: &Path,
    cargo_target: &str,
    profile: &str,
) -> Result<PathBuf> {
    let target_name = Path::new(cargo_target)
        .file_stem()
        .or_else(|| Path::new(cargo_target).file_name())
        .and_then(|s| s.to_str())
        .with_context(|| format!("invalid Cargo target `{cargo_target}`"))?;
    Ok(workspace_root
        .join("target")
        .join(target_name)
        .join(profile))
}

fn package_for_manifest<'a>(metadata: &'a Metadata, cargo_toml: &Path) -> Result<&'a Package> {
    let wanted = cargo_toml
        .canonicalize()
        .with_context(|| format!("canonicalize {}", cargo_toml.display()))?;
    metadata
        .packages
        .iter()
        .find(|package| package.manifest_path.as_std_path() == wanted)
        .with_context(|| {
            format!(
                "module manifest {} is not a workspace package",
                cargo_toml.display()
            )
        })
}

fn lib_target_name(package: &Package) -> Result<&str> {
    package
        .targets
        .iter()
        .find(|target| {
            target
                .kind
                .iter()
                .any(|kind| matches!(kind, TargetKind::Lib))
        })
        .map(|target| target.name.as_str())
        .with_context(|| format!("package `{}` does not define a lib target", package.name))
}

fn rust_crate_file_stem(name: &str) -> String {
    name.replace('-', "_")
}

fn inject_modules_into_rootfs(
    workspace_root: &Path,
    rootfs: &Path,
    modules: &[PathBuf],
) -> Result<()> {
    if modules.is_empty() {
        return Ok(());
    }
    let rootfs = if rootfs.is_absolute() {
        rootfs.to_path_buf()
    } else {
        workspace_root.join(rootfs)
    };
    if !rootfs.exists() {
        bail!("rootfs image not found: {}", rootfs.display());
    }

    let overlay_dir = TempOverlayDir::new()?;
    let modules_dir = overlay_dir.path().join("modules");
    fs::create_dir_all(&modules_dir)
        .with_context(|| format!("failed to create {}", modules_dir.display()))?;

    for module in modules {
        let file_name = module
            .file_name()
            .with_context(|| format!("invalid module path {}", module.display()))?;
        let dest = modules_dir.join(file_name);
        fs::copy(module, &dest).with_context(|| {
            format!("failed to copy {} to {}", module.display(), dest.display())
        })?;
    }

    crate::rootfs::inject::inject_overlay(&rootfs, overlay_dir.path())?;
    println!(
        "[kmod] injected {} module(s) into {}:/modules",
        modules.len(),
        rootfs.display()
    );
    Ok(())
}

struct TempOverlayDir {
    path: PathBuf,
}

impl TempOverlayDir {
    fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();

        for attempt in 0..100 {
            let path = base.join(format!("axbuild-kmod-overlay-{pid}-{nanos}-{attempt}"));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(err)
                        .with_context(|| format!("failed to create {}", path.display()));
                }
            }
        }

        bail!("failed to create a unique temporary kmod overlay directory");
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempOverlayDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Linker (and any leading args) for the partial-link (`-r`) step.
///
/// The toolchain ships `rust-lld`, but invoked under that name it is a
/// *generic* lld driver and refuses a direct `-r -T ...` invocation — it exits
/// asking to be called as `ld.lld`/`ld64.lld`/`lld-link`/`wasm-ld`. Rather than
/// depend on a separately-installed `ld.lld` symlink (absent in the build
/// container / CI), we drive the bundled `rust-lld` as the GNU ELF driver via
/// `-flavor gnu`, which handles all four (ELF) targets from one host binary.
/// Callers may override the whole program via the `KMOD_LINKER` environment
/// variable (e.g. to a GNU `ld`), in which case no flavor args are injected.
fn pick_linker(_target_triple: &str) -> (&'static str, &'static [&'static str]) {
    ("rust-lld", &["-flavor", "gnu"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_arg_value_removes_bin_and_value() {
        let mut args = vec![
            "-Z".to_string(),
            "build-std=core,alloc".to_string(),
            "--bin".to_string(),
            "starryos".to_string(),
            "--message-format".to_string(),
            "json".to_string(),
        ];

        remove_arg_value(&mut args, "--bin");

        assert_eq!(
            args,
            vec![
                "-Z".to_string(),
                "build-std=core,alloc".to_string(),
                "--message-format".to_string(),
                "json".to_string()
            ]
        );
    }

    #[test]
    fn cargo_target_output_dir_uses_json_file_stem() {
        let path = cargo_target_output_dir(
            Path::new("/ws"),
            "scripts/targets/std/riscv64gc-unknown-linux-musl.json",
            "release",
        )
        .unwrap();

        assert_eq!(
            path,
            Path::new("/ws")
                .join("target")
                .join("riscv64gc-unknown-linux-musl")
                .join("release")
        );
    }

    #[test]
    fn rust_crate_file_stem_replaces_hyphens() {
        assert_eq!(rust_crate_file_stem("my-module"), "my_module");
    }

    #[test]
    fn linux_c_module_names_parse_obj_m_entries() {
        let dir = tempfile::tempdir().unwrap();
        let makefile = dir.path().join("Makefile");
        fs::write(
            &makefile,
            r#"
obj-m += linux-hello.o
obj-m += second.o # comment
ccflags-remove-y += -pg
"#,
        )
        .unwrap();

        assert_eq!(
            linux_c_module_names(&makefile).unwrap(),
            vec!["linux-hello".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn discover_modules_finds_rust_and_linux_c_modules() {
        let dir = tempfile::tempdir().unwrap();
        let rust_module = dir.path().join("rust-module");
        let c_module = dir.path().join("linux-module");
        fs::create_dir_all(&rust_module).unwrap();
        fs::create_dir_all(&c_module).unwrap();
        fs::write(rust_module.join("Cargo.toml"), "[package]\nname = \"m\"\n").unwrap();
        fs::write(c_module.join("Makefile"), "obj-m += linux-hello.o\n").unwrap();

        let modules = discover_modules(dir.path()).unwrap();

        assert!(modules.contains(&ModuleSpec::Rust(rust_module)));
        assert!(modules.contains(&ModuleSpec::LinuxC(c_module)));
    }

    #[test]
    fn linux_c_module_skips_when_arch_differs_from_host() {
        let module = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let mismatched_arch = match normalized_host_arch() {
            "x86_64" => "aarch64",
            _ => "x86_64",
        };

        let built = build_one_linux_c_module(module.path(), mismatched_arch, out.path()).unwrap();

        assert!(built.is_none());
    }

    #[test]
    fn linux_c_make_args_pass_module_pwd_for_makefile_pwd_users() {
        let module_path = Path::new("/ws/os/StarryOS/lkm/linux-hello");
        let mut clean_args = linux_c_module_make_args(module_path);
        clean_args.push("clean".to_string());

        assert_eq!(
            linux_c_module_make_args(module_path),
            vec![
                "-C".to_string(),
                "/ws/os/StarryOS/lkm/linux-hello".to_string(),
                "PWD=/ws/os/StarryOS/lkm/linux-hello".to_string()
            ]
        );
        assert_eq!(
            clean_args,
            vec![
                "-C".to_string(),
                "/ws/os/StarryOS/lkm/linux-hello".to_string(),
                "PWD=/ws/os/StarryOS/lkm/linux-hello".to_string(),
                "clean".to_string()
            ]
        );
    }
}
