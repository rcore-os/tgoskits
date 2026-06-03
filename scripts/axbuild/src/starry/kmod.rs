//! `cargo xtask starry kmod build` — build a StarryOS loadable kernel
//! module (`.ko`) from a Rust crate without involving make/Makefile.
//!
//! Replaces the upstream `Starry-OS/StarryOS:ebpf-kmod`
//! `modules/kmod.mk` + `make/build.mk` pipeline (per
//! `WORKFLOW_EBPF_LKM_MIGRATION.md §5.3` — "do **not** introduce
//! `Makefile`, `make/`, or `modules/kmod.mk`"). The build flow is:
//!
//! 1. Run `cargo build --release -p <module>` against the kernel target
//!    triple, producing an rlib archive.
//! 2. Invoke the GNU `ld` linker with `-r` (partial / relocatable link)
//!    and the StarryOS kmod linker script
//!    (`os/StarryOS/scripts/kmod-linker.ld`), turning the rlib into a
//!    `.ko` ELF.
//! 3. Drop the result into `target/<arch>/kmod/<module>.ko`.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::starry::Starry;

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
    /// Target arch (`aarch64` / `riscv64` / `x86_64` / `loongarch64`).
    #[arg(long, default_value = "x86_64")]
    pub arch: String,

    /// Path to a module crate (or directory containing module crates,
    /// auto-discovered via `Cargo.toml` lookup, depth ≤ 10). May be
    /// repeated.
    #[arg(short = 'm', long, value_name = "PATH")]
    pub module: Vec<PathBuf>,

    /// Build every module crate under `os/StarryOS/modules/`.
    #[arg(long, conflicts_with = "module")]
    pub all: bool,
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
        let module_paths = collect_module_paths(&workspace_root, &args)?;
        if module_paths.is_empty() {
            bail!("no module crates found (use `--module <path>` or `--all`)");
        }

        let target_triple = target_triple_for_arch(&args.arch)?;
        let linker_script = workspace_root.join("os/StarryOS/scripts/kmod-linker.ld");
        if !linker_script.exists() {
            bail!(
                "kmod linker script not found at {}",
                linker_script.display()
            );
        }

        let out_root = workspace_root.join(format!("target/{}/kmod", args.arch));
        std::fs::create_dir_all(&out_root)
            .with_context(|| format!("create {}", out_root.display()))?;

        for module_path in module_paths {
            build_one_module(
                &workspace_root,
                &module_path,
                target_triple,
                &linker_script,
                &out_root,
            )?;
        }
        Ok(())
    }
}

fn target_triple_for_arch(arch: &str) -> Result<&'static str> {
    Ok(match arch {
        "x86_64" => "x86_64-unknown-none",
        "aarch64" => "aarch64-unknown-none-softfloat",
        "riscv64" => "riscv64gc-unknown-none-elf",
        "loongarch64" => "loongarch64-unknown-none-softfloat",
        other => bail!("unsupported arch: {other}"),
    })
}

fn collect_module_paths(workspace_root: &Path, args: &ArgsKmodBuild) -> Result<Vec<PathBuf>> {
    if args.all {
        let modules_dir = workspace_root.join("os/StarryOS/modules");
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
            paths.push(resolved);
        } else {
            paths.extend(discover_modules(&resolved)?);
        }
    }
    Ok(paths)
}

fn discover_modules(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    discover_modules_inner(root, 0, &mut out)?;
    out.sort();
    out.dedup();
    Ok(out)
}

fn discover_modules_inner(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    if depth > 10 {
        return Ok(());
    }
    if dir.join("Cargo.toml").exists() {
        out.push(dir.to_path_buf());
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

fn build_one_module(
    workspace_root: &Path,
    module_path: &Path,
    target_triple: &str,
    linker_script: &Path,
    out_dir: &Path,
) -> Result<()> {
    let cargo_toml = module_path.join("Cargo.toml");
    if !cargo_toml.exists() {
        bail!("missing {}", cargo_toml.display());
    }
    let module_name = module_path
        .file_name()
        .and_then(|s| s.to_str())
        .with_context(|| format!("invalid module path {}", module_path.display()))?
        .to_string();

    println!("[kmod] building {module_name} for {target_triple}");

    // Step 1: cargo build the module crate into an rlib.
    let status = Command::new("cargo")
        .args(["build", "--release", "--manifest-path"])
        .arg(&cargo_toml)
        .arg("--target")
        .arg(target_triple)
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("invoke cargo build for {module_name}"))?;
    if !status.success() {
        bail!("cargo build failed for {module_name}");
    }

    // Step 2: locate the produced rlib. `cargo build` puts rlibs at
    // `target/<triple>/release/lib<crate>.rlib`. We expect the module
    // crate's lib name to match the module's file_name (most kmod
    // examples are set up that way).
    let rlib_name = format!("lib{}.rlib", module_name.replace('-', "_"));
    let rlib_path = workspace_root
        .join("target")
        .join(target_triple)
        .join("release")
        .join(&rlib_name);
    if !rlib_path.exists() {
        bail!(
            "expected rlib not found at {} — does the crate's [lib] name match the directory name?",
            rlib_path.display()
        );
    }

    // Step 3: partial-link into a .ko via the kmod linker script.
    let ko_path = out_dir.join(format!("{module_name}.ko"));
    // `(program, leading_args)`: the default ships `-flavor gnu` so `rust-lld`
    // runs as the GNU ELF driver; a `KMOD_LINKER` override is used verbatim.
    let (linker, lead_args): (String, &[&str]) = match std::env::var("KMOD_LINKER") {
        Ok(l) => (l, &[]),
        Err(_) => {
            let (prog, args) = pick_linker(target_triple);
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
    Ok(())
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
