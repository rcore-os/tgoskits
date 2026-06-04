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

/// The kernel binary package the module is co-built with for hash parity (see
/// `build_one_module`). StarryOS modules link against `starry_kernel`, which is
/// `starryos`'s library; co-building under `starryos`'s features pins the shared
/// crates to the running kernel's exact configuration.
const KERNEL_PKG: &str = "starryos";

/// Features the kernel resolves to for the x86-pc QEMU platform — the module is
/// co-built with these so `cargo` unifies `starry_kernel` (and its dependency
/// closure) to the identical feature set, hence identical crate hashes. `qemu`
/// is qualified to `starryos/` so the module's own same-named feature is not
/// also activated (which would add features to the shared closure and diverge
/// the hash). Matches the resolved feature set of `cargo xtask starry build`.
const KERNEL_KMOD_FEATURES: &str = "ax-hal/x86-pc,starryos/qemu";

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

    // This loadable-module pipeline requires the kernel to have been built in
    // `STARRY_KMOD` mode (`STARRY_KMOD=y cargo xtask starry build`): `lto=false`
    // + `static`/`large` codegen so its `.kallsyms` retains every symbol the
    // module relocates against. The module is co-built into the *same* target
    // dir with the *same* features/flags, so cargo reuses the kernel's crate
    // artifacts and the `.ko`'s symbol hashes match the running kernel exactly.
    if target_triple != "x86_64-unknown-none" {
        bail!(
            "loadable kmod build currently supports only `--arch x86_64` (kernel feature/codegen \
             parity is wired for the x86-pc platform)"
        );
    }

    // Step 1: cargo build the module crate into an rlib.
    //
    // This must mirror the *kernel's* build configuration — the same JSON
    // target spec plus `-Z build-std=core,alloc` — so the module's `core`
    // and `alloc` (and everything monomorphized out of them) get the SAME
    // crate-disambiguator hash as the running kernel. The kmod loader
    // resolves a module's relocations against the kernel kallsyms table by
    // exact (mangled) symbol name; a plain `cargo build` links the
    // *precompiled sysroot* `core`/`alloc`, whose hash differs from the
    // build-std kernel, so every `core::fmt` / `alloc` symbol the module
    // references (`core::fmt::write`, `unwrap_failed`, `handle_alloc_error`,
    // …) would fail to resolve at load time. x86_64 has no PIE target, and
    // the static-platform Starry kernel is built no-pie on every arch, so
    // the module uses the no-pie spec to match.
    let target_json = crate::build::cargo_target_json_path(target_triple, false)?;
    let target_json = target_json.display().to_string();
    // Override the kernel spec's `relocation-model = "pic"`: PIC routes every
    // external-symbol access through the GOT and emits relaxable GOT relocations
    // (e.g. x86_64 `R_X86_64_REX_GOTPCRELX`, type 42) that the kmod loader's
    // minimal relocator does not implement. A loaded module instead needs plain
    // absolute/PC-relative relocations. `relocation-model=static` drops the GOT,
    // and `code-model=large` makes symbol references 64-bit absolute
    // (`R_X86_64_64`) so they can reach the high (`0xffff_8000_…`) kernel/module
    // load addresses without 32-bit-displacement overflow — both are types the
    // loader supports. These are pure codegen flags: they do not change the
    // crate-disambiguator hash, so the build-std `core`/`alloc` symbol *names*
    // still match the kernel and resolve against `.kallsyms`.
    //
    // `-Cembed-bitcode=no` (paired with `profile.release.lto=false` below) is
    // what makes the final partial-link a plain object concatenation instead of
    // an LTO step. The workspace's `lto = true` would otherwise leave LTO
    // bitcode in every rlib, and `rust-lld -r` then re-runs the LLVM module
    // verifier on the merged bitcode — which rejects the hand-written
    // allocator-shim symbols the module supplies (the verifier insists
    // `__rust_alloc_zeroed` belong to `__rust_alloc`'s "alloc-family", a
    // linkage the `#[global_allocator]` macro establishes but a manual shim
    // cannot). With no bitcode there is no LTO and no re-verification; the
    // shims' (perfectly valid) machine code links as-is. Neither flag changes
    // the crate-disambiguator hash, so symbol names still match the kernel.
    let module_rustflags = [
        "-Crelocation-model=static".to_string(),
        "-Ccode-model=large".to_string(),
    ];
    let module_pkg = manifest_package_name(&cargo_toml)?;
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("-p")
        .arg(KERNEL_PKG)
        .arg("-p")
        .arg(&module_pkg)
        .arg("--target")
        .arg(&target_json)
        .args(crate::build::BuildInfo::build_cargo_args(
            &target_json,
            &module_rustflags,
        ))
        .arg("--features")
        .arg(KERNEL_KMOD_FEATURES)
        // Match the `STARRY_KMOD` kernel build so the shared crates (and thus
        // their `StableCrateId` hashes) are identical and get reused: no LTO
        // (retain symbols), and the platform/`qemu` features above. `qemu` is
        // qualified to `starryos/` so the module's own same-named feature is not
        // additionally activated (which would perturb the shared feature
        // closure). See `build::kmod_build_mode`.
        .env("CARGO_UNSTABLE_JSON_TARGET_SPEC", "true")
        .env("CARGO_PROFILE_RELEASE_LTO", "false");
    // The module rlib (what we actually need) and where co-building writes it.
    // With `--target <triple>.json`, cargo puts target artifacts under
    // `<CARGO_TARGET_DIR>/<triple>/release/` (own rlib) and `.../release/deps/`.
    let release_dir = workspace_root
        .join("target")
        .join(target_triple)
        .join("release");
    let rlib_name = format!("lib{}.rlib", module_name.replace('-', "_"));
    let rlib_path = release_dir.join(&rlib_name);
    // Remove any stale rlib so its post-build presence unambiguously means this
    // invocation produced it.
    let _ = std::fs::remove_file(&rlib_path);

    let status = cmd
        .current_dir(workspace_root)
        .status()
        .with_context(|| format!("invoke cargo build for {module_name}"))?;

    // Step 2: locate the produced rlib. Co-building the kernel binary
    // (`-p starryos`) only pins the shared crates' features for hash parity; the
    // kernel *bin link* itself fails under a raw `cargo build` (`cannot find
    // linker script axplat.x` — that scaffolding is set up only by the
    // ostool/xtask kernel-build path, and is irrelevant here). cargo still
    // compiles the module rlib and every shared dependency before that final
    // link, so a non-zero exit is expected and fine **iff** the rlib was built.
    if !rlib_path.exists() {
        if !status.success() {
            bail!(
                "cargo build failed for {module_name} and produced no rlib at {} — see the cargo \
                 error above (a real module compile error, not the tolerated kernel-bin link \
                 failure)",
                rlib_path.display()
            );
        }
        bail!(
            "expected rlib not found at {} — does the crate's [lib] name match the directory name?",
            rlib_path.display()
        );
    }
    if !status.success() {
        println!(
            "[kmod] note: kernel-bin link failed as expected under raw cargo; using module rlib \
             {} (produced before the link step)",
            rlib_path.display()
        );
    }
    // Step 3: partial-link the module's own rlib into a `.ko` via the kmod
    // linker script. `--whole-archive` keeps all of the module's objects (the
    // `#[init_fn]`/`#[exit_fn]`/`module!` markers are reachable only from the
    // linker-script `KEEP`s); every dependency symbol stays undefined and is
    // relocated against the kernel `.kallsyms` by the loader at load time. No
    // dependencies are bundled — the `STARRY_KMOD` kernel retains them all.
    let ko_path = out_dir.join(format!("{module_name}.ko"));
    let (linker, lead_args): (String, Vec<String>) = resolve_linker(target_triple)?;
    let status = Command::new(&linker)
        .args(&lead_args)
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

/// Read the `name = "..."` of the `[package]` table from a crate manifest.
fn manifest_package_name(cargo_toml: &Path) -> Result<String> {
    let text = std::fs::read_to_string(cargo_toml)
        .with_context(|| format!("read {}", cargo_toml.display()))?;
    let mut in_package = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('[') {
            in_package = rest.trim_end_matches(']').trim() == "package";
            continue;
        }
        if in_package && let Some(rest) = trimmed.strip_prefix("name") {
            // `name = "hello"` → strip `=`, whitespace and quotes.
            let val = rest.trim_start().trim_start_matches('=').trim();
            return Ok(val.trim_matches('"').to_string());
        }
    }
    bail!("no [package] name in {}", cargo_toml.display())
}

/// Linker program (and any leading args) for the partial-link (`-r`) step.
///
/// The toolchain ships `rust-lld`, but invoked under that name it is a
/// *generic* lld driver and refuses a direct `-r -T ...` invocation — it exits
/// asking to be called as `ld.lld`/`ld64.lld`/`lld-link`/`wasm-ld`. We drive it
/// as the GNU ELF driver via `-flavor gnu`, which handles all four (ELF)
/// targets from one host binary.
///
/// It must be the `rust-lld` of the **active toolchain**, not whatever
/// `rust-lld` happens to be first on `PATH` (e.g. a stale `cargo-binutils`
/// shim): the module rlibs embed LTO bitcode tagged with the toolchain's LLVM
/// version, and a mismatched lld rejects it ("Unknown attribute kind …"). We
/// therefore resolve it under the sysroot reported by `rustc`. Callers may
/// override the whole program via the `KMOD_LINKER` environment variable (e.g.
/// to a GNU `ld`), in which case no flavor args are injected.
fn resolve_linker(_target_triple: &str) -> Result<(String, Vec<String>)> {
    if let Ok(l) = std::env::var("KMOD_LINKER") {
        return Ok((l, Vec::new()));
    }
    let flavor = vec!["-flavor".to_string(), "gnu".to_string()];
    match toolchain_rust_lld() {
        Some(path) => Ok((path.display().to_string(), flavor)),
        // Last resort: trust `PATH`. Correct when the active toolchain's
        // `rust-lld` is the one on `PATH`; otherwise the bitcode-version check
        // above will surface a clear error.
        None => Ok(("rust-lld".to_string(), flavor)),
    }
}

/// Locate the active toolchain's `rust-lld`, i.e.
/// `$(rustc --print sysroot)/lib/rustlib/<host>/bin/rust-lld`.
fn toolchain_rust_lld() -> Option<PathBuf> {
    let sysroot = Command::new("rustc")
        .arg("--print")
        .arg("sysroot")
        .output()
        .ok()?;
    if !sysroot.status.success() {
        return None;
    }
    let sysroot = PathBuf::from(String::from_utf8(sysroot.stdout).ok()?.trim());

    let vv = Command::new("rustc").arg("-vV").output().ok()?;
    let host = String::from_utf8(vv.stdout)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(|h| h.trim().to_string()))?;

    let candidate = sysroot.join("lib/rustlib").join(&host).join("bin/rust-lld");
    candidate.exists().then_some(candidate)
}
