use super::*;

pub(crate) fn case_rust_source_dir(case: &TestQemuCase) -> PathBuf {
    case.case_dir.join("rust")
}

/// Maps a StarryOS arch name to the corresponding Rust musl target triple.
pub(super) fn rust_musl_target(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        "riscv64" => Ok("riscv64gc-unknown-linux-musl"),
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "loongarch64" => Ok("loongarch64-unknown-linux-musl"),
        _ => bail!(
            "Rust-based QEMU test cases are only supported on aarch64, riscv64, x86_64, and \
             loongarch64, but got `{arch}`"
        ),
    }
}

/// Prepares overlay assets for a Rust-based QEMU test case.
///
/// This pipeline:
/// 1. Cross-compiles the Rust project in `rust/` using `cargo build --release`
///    targeting the appropriate musl triple for the guest architecture.
/// 2. Copies the resulting static binary into the overlay at `/usr/bin/`.
/// 3. Injects the overlay into the rootfs image.
///
/// The binary name is taken from the Cargo.toml `[[bin]]` name, or falls back
/// to the package name.  The `rust/` directory must contain a `Cargo.toml`.
pub(crate) fn prepare_rust_case_assets_sync(
    arch: &str,
    case: &TestQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
    config: &CaseAssetConfig,
) -> anyhow::Result<()> {
    let rust_dir = case_rust_source_dir(case);
    ensure!(
        rust_dir.is_dir(),
        "missing case Rust source directory `{}`",
        rust_dir.display()
    );
    let cargo_toml = rust_dir.join("Cargo.toml");
    ensure!(
        cargo_toml.is_file(),
        "missing Cargo.toml in Rust case source directory `{}`",
        rust_dir.display()
    );

    let target_triple = rust_musl_target(arch)?;

    case_assets::reset_dir(&layout.overlay_dir)?;
    case_assets::reset_dir(&layout.staging_root)?;
    case_assets::reset_dir(&layout.command_wrapper_dir)?;
    case_assets::reset_dir(&layout.cross_bin_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    // Ensure the musl target is installed in the active toolchain.
    let mut add_target = Command::new("rustup");
    add_target.arg("target").arg("add").arg(target_triple);
    add_target
        .exec()
        .with_context(|| format!("failed to install Rust target `{target_triple}` via rustup"))?;

    // Extract the rootfs so we can use the Alpine cross-linker for architectures
    // whose ELF format the host linker cannot handle (e.g. loongarch64).
    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
    (config.prepare_staging_root)(&layout.staging_root)?;
    write_musl_loader_search_path(arch, &layout.staging_root)?;

    // Build a qemu-user wrapper for the cross-linker from the Alpine sysroot.
    let spec = cross_compile_spec(arch)?;
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_cross_bin_wrappers(layout, spec, &qemu_runner)?;

    // Run prebuild.sh if present — runs inside the Alpine staging root via
    // qemu-user, same as C cases.  Use this to install native deps (e.g.
    // `apk add dbus-dev`) that the cargo build needs via pkg-config.
    let prebuild_script = case_rust_prebuild_script_path(case);
    if prebuild_script.is_file() {
        let extra_script_envs = prepare_guest_package_env(config, &layout.staging_root)?;
        let prebuild_env =
            prepare_guest_prebuild_env(arch, case, layout, extra_script_envs, config)?;
        let mut command = build_prebuild_command(case, &prebuild_script, layout, &prebuild_env)?;
        // Override current_dir to rust/ — build_prebuild_command defaults to c/.
        command.current_dir(&rust_dir);
        command
            .exec()
            .with_context(|| format!("failed to run rust case prebuild.sh for `{}`", case.name))?;
    }

    // The linker env var name is CARGO_TARGET_<UPPER_TRIPLE>_LINKER.
    let linker_env_key = format!(
        "CARGO_TARGET_{}_LINKER",
        target_triple.to_uppercase().replace('-', "_")
    );
    let linker_path = layout.cross_bin_dir.join("ld");

    // Cross-compile the Rust project for the musl target.
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("--target")
        .arg(target_triple)
        .arg("--manifest-path")
        .arg(&cargo_toml)
        .arg("--target-dir")
        .arg(&layout.build_dir)
        .env("RUSTFLAGS", "-C target-feature=+crt-static")
        .env(&linker_env_key, &linker_path)
        // Point pkg-config at the Alpine sysroot so crates with native deps
        // (e.g. dbus via keyring) can find their .pc files when cross-compiling.
        .env(
            "PKG_CONFIG_LIBDIR",
            format!(
                "{}:{}",
                layout.staging_root.join("usr/lib/pkgconfig").display(),
                layout.staging_root.join("usr/share/pkgconfig").display()
            ),
        )
        .env(
            "PKG_CONFIG_SYSROOT_DIR",
            layout.staging_root.display().to_string(),
        )
        .env("PKG_CONFIG_PATH", "");
    cmd.exec().with_context(|| {
        format!(
            "failed to cross-compile Rust case `{}` for target `{target_triple}`",
            case.name
        )
    })?;

    // Discover the binary name from Cargo.toml.
    let bin_name = rust_case_bin_name(&cargo_toml)?;

    // The compiled binary lives at <build_dir>/<target_triple>/release/<bin_name>.
    let bin_src = layout
        .build_dir
        .join(target_triple)
        .join("release")
        .join(&bin_name);
    ensure!(
        bin_src.is_file(),
        "expected compiled Rust binary at `{}` but it was not found",
        bin_src.display()
    );

    // Install the binary into the overlay at /usr/bin/.
    let dest_bin_dir = layout.overlay_dir.join("usr/bin");
    fs::create_dir_all(&dest_bin_dir)
        .with_context(|| format!("failed to create {}", dest_bin_dir.display()))?;
    let bin_dst = dest_bin_dir.join(&bin_name);
    fs::copy(&bin_src, &bin_dst).with_context(|| {
        format!(
            "failed to copy {} to {}",
            bin_src.display(),
            bin_dst.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&bin_dst)
            .with_context(|| format!("failed to stat {}", bin_dst.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&bin_dst, perms)
            .with_context(|| format!("failed to chmod {}", bin_dst.display()))?;
    }

    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

/// Reads the binary name from a `Cargo.toml`.
///
/// Returns the first `[[bin]]` name if present, otherwise the `[package]` name.
pub(super) fn rust_case_bin_name(cargo_toml: &Path) -> anyhow::Result<String> {
    #[derive(serde::Deserialize)]
    struct CargoToml {
        package: Option<CargoPackage>,
        bin: Option<Vec<CargoBin>>,
    }
    #[derive(serde::Deserialize)]
    struct CargoPackage {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct CargoBin {
        name: String,
    }

    let content = fs::read_to_string(cargo_toml)
        .with_context(|| format!("failed to read {}", cargo_toml.display()))?;
    let manifest: CargoToml = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", cargo_toml.display()))?;

    if let Some(bins) = manifest.bin
        && let Some(first) = bins.into_iter().next()
    {
        return Ok(first.name);
    }

    manifest.package.map(|p| p.name).ok_or_else(|| {
        anyhow::anyhow!(
            "no `[package]` or `[[bin]]` found in {}",
            cargo_toml.display()
        )
    })
}
