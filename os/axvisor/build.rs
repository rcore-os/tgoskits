// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! This build script reads config file paths from the `AXVISOR_VM_CONFIGS` environment variable,
//! reads them, and then outputs them to `$(OUT_DIR)/vm_configs.rs` to be used by
//! `src/runtime/config.rs`.
//!
//! The `AXVISOR_VM_CONFIGS` environment variable should follow the format convention for the `PATH`
//! environment variable on the building platform, i.e., paths are separated by colons (`:`) on
//! Unix-like systems and semicolons (`;`) on Windows.
//!
//! In the generated `vm_configs.rs` file, a function `static_vm_configs` is defined that returns a
//! `Vec<&'static str>` containing the contents of the configuration files.
//!
//! If the `AXVISOR_VM_CONFIGS` environment variable is not set, `static_vm_configs` will call the
//! `default_static_vm_configs` function from `src/runtime/config.rs` to return the default
//! configurations.
//!
//! If the `AXVISOR_VM_CONFIGS` environment variable is set but the configuration files cannot be
//! read, the build script will output a `compile_error!` macro that will cause the build to fail.
//!
//! A function `get_memory_images` is also provided to get every vm image from the configuration
//! files.
//!
//! This build script reruns if the `AXVISOR_VM_CONFIGS` environment variable changes, or if the
//! `build.rs` file changes, or if any of the files in the paths specified by `AXVISOR_VM_CONFIGS`
//! change.
use std::{
    env,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use quote::quote;
use syn::LitStr;
use toml::Table;

fn fallback_platform_for_arch(arch: &str) -> &'static str {
    match arch {
        "aarch64" => "aarch64-generic",
        "loongarch64" => "loongarch64-plat-dyn",
        "x86_64" => "dummy",
        "riscv64" => "riscv64-plat-dyn",
        _ => "dummy",
    }
}

/// A configuration file that has been read from disk.
struct ConfigFile {
    /// The path to the configuration file.
    pub path: OsString,
    /// The contents of the configuration file.
    pub content: String,
}

/// Gets the paths (colon-separated) from the `AXVISOR_VM_CONFIGS` environment variable.
///
/// Returns `None` if the environment variable is not set.
fn get_config_paths() -> Option<Vec<OsString>> {
    env::var("AXVISOR_VM_CONFIGS")
        .ok()
        .map(|paths| env::split_paths(&paths).map(OsString::from).collect())
}

/// Gets the paths and contents of the configuration files specified by the `AXVISOR_VM_CONFIGS` environment variable.
///
/// Returns a tuple of the paths and contents of the configuration files if successful, or an error message if not.
fn get_configs() -> Result<Vec<ConfigFile>, String> {
    get_config_paths()
        .map(|paths| {
            paths
                .into_iter()
                .map(|path| {
                    let path_buf = PathBuf::from(&path);
                    let content = fs::read_to_string(&path_buf).map_err(|e| {
                        format!("Failed to read file {}: {}", path_buf.display(), e)
                    })?;
                    Ok(ConfigFile { path, content })
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(vec![]))
}

/// Opens the output file for writing.
///
/// Returns the file handle.
fn open_output_file() -> fs::File {
    let output_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set by Cargo"));
    let output_file = output_dir.join("vm_configs.rs");

    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_file)
        .expect("failed to open generated vm_configs.rs")
}

fn write_tokens(out_file: &mut fs::File, tokens: proc_macro2::TokenStream) -> anyhow::Result<()> {
    let syntax_tree = syn::parse2(tokens)?;
    let formatted = prettyplease::unparse(&syntax_tree);
    out_file.write_all(formatted.as_bytes())?;

    Ok(())
}

fn resolve_config_path(configs_path: impl AsRef<Path>, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    configs_path
        .as_ref()
        .parent()
        .map(|parent| parent.join(path))
        .unwrap_or_else(|| path.to_path_buf())
}

struct MemoryImage {
    pub id: usize,
    pub kernel: PathBuf,
    pub dtb: Option<PathBuf>,
    pub bios: Option<PathBuf>,
    pub ramdisk: Option<PathBuf>,
}

struct FirmwareImage {
    pub id: usize,
    pub bios: PathBuf,
}

fn boot_firmware_path(kernel_config: &Table, enable_bios: bool) -> Option<&str> {
    if !enable_bios {
        return None;
    }

    let bios_path = || kernel_config.get("bios_path").and_then(|v| v.as_str());
    let uefi_firmware_path = || {
        kernel_config
            .get("uefi_firmware_path")
            .and_then(|v| v.as_str())
    };

    match kernel_config.get("boot_protocol").and_then(|v| v.as_str()) {
        Some("uefi" | "efi") => uefi_firmware_path().or_else(bios_path),
        Some("direct" | "kernel") => None,
        _ => bios_path(),
    }
}

fn parse_config_file(config_file: &ConfigFile) -> Option<MemoryImage> {
    let config = config_file.content.parse::<Table>().ok()?;

    let id = config.get("base")?.as_table()?.get("id")?.as_integer()? as usize;

    let image_location_val = config.get("kernel")?.as_table()?.get("image_location")?;

    let image_location = image_location_val.as_str()?;

    if image_location != "memory" {
        return None;
    }

    let kernel_path = config.get("kernel")?.as_table()?.get("kernel_path")?;

    let kernel = resolve_config_path(&config_file.path, kernel_path.as_str()?);

    let dtb = config
        .get("kernel")?
        .as_table()?
        .get("dtb_path")
        .and_then(|v| v.as_str())
        .map(|v| resolve_config_path(&config_file.path, v));

    let enable_bios = config
        .get("kernel")?
        .as_table()?
        .get("enable_bios")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let kernel_config = config.get("kernel")?.as_table()?;

    let bios = boot_firmware_path(kernel_config, enable_bios)
        .map(|v| resolve_config_path(&config_file.path, v));

    let ramdisk = kernel_config
        .get("ramdisk_path")
        .and_then(|v| v.as_str())
        .map(|v| resolve_config_path(&config_file.path, v));

    Some(MemoryImage {
        id,
        kernel,
        dtb,
        bios,
        ramdisk,
    })
}

fn parse_firmware_config_file(config_file: &ConfigFile) -> Option<FirmwareImage> {
    let config = config_file.content.parse::<Table>().ok()?;
    let id = config.get("base")?.as_table()?.get("id")?.as_integer()? as usize;
    let kernel_config = config.get("kernel")?.as_table()?;
    let enable_bios = kernel_config
        .get("enable_bios")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let bios = boot_firmware_path(kernel_config, enable_bios)
        .map(|v| resolve_config_path(&config_file.path, v))?;

    Some(FirmwareImage { id, bios })
}

/// Generate function to load guest images from config
/// Toml file must be provided to load from memory.
fn generate_guest_img_loading_functions(
    out_file: &mut fs::File,
    config_files: Vec<ConfigFile>,
) -> anyhow::Result<()> {
    let mut memory_images = vec![];

    for config_file in config_files {
        if let Some(files) = parse_config_file(&config_file) {
            let id = files.id;
            let kernel = files
                .kernel
                .canonicalize()
                .with_context(|| format!("Path {} not found", files.kernel.display()))?
                .display()
                .to_string();
            let dtb = match files.dtb {
                Some(v) => {
                    let s = v
                        .canonicalize()
                        .with_context(|| format!("Path {} not found", v.display()))?
                        .display()
                        .to_string();
                    quote! { Some(include_bytes!(#s)) }
                }
                None => quote! { None },
            };

            let bios = match files.bios {
                Some(v) => {
                    let s = v
                        .canonicalize()
                        .with_context(|| format!("Path {} not found", v.display()))?
                        .display()
                        .to_string();
                    quote! { Some(include_bytes!(#s)) }
                }
                None => quote! { None },
            };

            let ramdisk = match files.ramdisk {
                Some(v) => {
                    let s = v
                        .canonicalize()
                        .with_context(|| format!("Path {} not found", v.display()))?
                        .display()
                        .to_string();
                    quote! { Some(include_bytes!(#s)) }
                }
                None => quote! { None },
            };

            memory_images.push(quote! {
                axvm::boot::StaticVmImage {
                    id: #id,
                    kernel: include_bytes!(#kernel),
                    dtb: #dtb,
                    bios: #bios,
                    ramdisk: #ramdisk,
                }
            });
        }
    }

    let output = quote! {
        /// Get memory images from config file.
        pub fn get_memory_images() -> &'static [axvm::boot::StaticVmImage] {
            &[
                #(#memory_images),*
            ]
        }
    };
    write_tokens(out_file, output)?;

    Ok(())
}

fn generate_firmware_img_loading_functions(
    out_file: &mut fs::File,
    config_files: &[ConfigFile],
) -> anyhow::Result<()> {
    let mut firmware_images = vec![];

    for config_file in config_files {
        if let Some(files) = parse_firmware_config_file(config_file) {
            let id = files.id;
            let Ok(bios) = files.bios.canonicalize() else {
                continue;
            };
            let bios = bios.display().to_string();

            firmware_images.push(quote! {
                axvm::boot::StaticVmImage {
                    id: #id,
                    kernel: &[],
                    dtb: None,
                    bios: Some(include_bytes!(#bios)),
                    ramdisk: None,
                }
            });
        }
    }

    let output = quote! {
        /// Get firmware images from config file.
        pub fn get_firmware_images() -> &'static [axvm::boot::StaticVmImage] {
            &[
                #(#firmware_images),*
            ]
        }
    };
    let syntax_tree = syn::parse2(output)?;
    let formatted = prettyplease::unparse(&syntax_tree);
    out_file.write_all(formatted.as_bytes())?;

    Ok(())
}

/// Minimum toolchain required to build the web UI from source.
const MIN_NODE_MAJOR: u32 = 24;
const MIN_NPM_MAJOR: u32 = 11;
const MIN_NPM_MINOR: u32 = 12; // npm >= 11.12.1

/// Hard limits on the embedded UI bundle (reviewer-imposed).
const MAX_UI_FILES: usize = 256;
const MAX_UI_GZ_BYTES: usize = 512 * 1024;
const MAX_UI_UNCOMPRESSED_BYTES: usize = 2 * 1024 * 1024;

/// Parse `"v24.19.0"` / `"24.19.0"` into `(major, minor)`.
fn semver_major_minor(v: &str) -> Option<(u32, u32)> {
    let v = v.trim_start_matches('v');
    let mut it = v.split('.');
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

/// Recursively collect `dist/` into `(relative path, is_dir, bytes)` entries.
fn collect_dist(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(PathBuf, bool, Vec<u8>)>,
    file_count: &mut usize,
    uncompressed: &mut usize,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            out.push((rel, true, Vec::new()));
            collect_dist(root, &path, out, file_count, uncompressed)?;
        } else if ft.is_file() {
            let data = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            *uncompressed += data.len();
            *file_count += 1;
            out.push((rel, false, data));
        }
    }
    Ok(())
}

/// Build the React web UI and pack `dist/` into a deterministic gzip tarball.
///
/// The tarball normalizes every header (mtime 0, mode `0o644`/`0o755`, uid/gid
/// 0, empty uname/gname) and the gzip stream fixes its mtime, so the embedded
/// bytes are reproducible from the same `dist/` contents. The result is written
/// to `OUT_DIR/web_ui_bundle.gz` and surfaced to the crate through the
/// generated `OUT_DIR/web_ui_bundle.rs` (`include_bytes!`).
fn build_web_ui_bundle() -> anyhow::Result<()> {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").context("CARGO_MANIFEST_DIR not set")?);
    let web_ui_dir = manifest_dir.join("web-ui");
    anyhow::ensure!(
        web_ui_dir.exists(),
        "web-ui directory missing: {}",
        web_ui_dir.display()
    );

    // Toolchain version gates.
    let node_ver = String::from_utf8(
        Command::new("node")
            .arg("--version")
            .output()
            .context("failed to run `node --version`; install Node.js >= 24")?
            .stdout,
    )
    .context("node version is not utf-8")?;
    let npm_ver = String::from_utf8(
        Command::new("npm")
            .arg("--version")
            .output()
            .context("failed to run `npm --version`; install npm >= 11.12.1")?
            .stdout,
    )
    .context("npm version is not utf-8")?;
    match semver_major_minor(&node_ver) {
        Some((m, _)) if m >= MIN_NODE_MAJOR => {}
        Some((m, _)) => anyhow::bail!("Node.js >= {MIN_NODE_MAJOR} required, found {m}"),
        None => anyhow::bail!("could not parse node version: {node_ver:?}"),
    }
    match semver_major_minor(&npm_ver) {
        Some((ma, mi)) if (ma, mi) >= (MIN_NPM_MAJOR, MIN_NPM_MINOR) => {}
        Some((ma, mi)) => {
            anyhow::bail!("npm >= {MIN_NPM_MAJOR}.{MIN_NPM_MINOR} required, found {ma}.{mi}")
        }
        None => anyhow::bail!("could not parse npm version: {npm_ver:?}"),
    }

    // Build from the committed lockfile (deterministic), then produce `dist/`.
    let ci = Command::new("npm")
        .args(["ci", "--no-audit", "--no-fund"])
        .current_dir(&web_ui_dir)
        .status()
        .context("failed to spawn `npm ci`")?;
    anyhow::ensure!(ci.success(), "`npm ci` failed with {ci}");
    let build = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&web_ui_dir)
        .status()
        .context("failed to spawn `npm run build`")?;
    anyhow::ensure!(build.success(), "`npm run build` failed with {build}");

    let dist = web_ui_dir.join("dist");
    anyhow::ensure!(dist.exists(), "web-ui build produced no dist/");

    let mut entries: Vec<(PathBuf, bool, Vec<u8>)> = Vec::new();
    let mut file_count = 0usize;
    let mut uncompressed = 0usize;
    collect_dist(
        &dist,
        &dist,
        &mut entries,
        &mut file_count,
        &mut uncompressed,
    )?;
    anyhow::ensure!(
        file_count <= MAX_UI_FILES,
        "web UI has {file_count} files; the hard limit is {MAX_UI_FILES}"
    );
    anyhow::ensure!(
        uncompressed <= MAX_UI_UNCOMPRESSED_BYTES,
        "web UI uncompressed size {uncompressed} exceeds the {MAX_UI_UNCOMPRESSED_BYTES} limit"
    );

    // Deterministic ordering independent of read_dir iteration order.
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    {
        let mut builder = tar::Builder::new(&mut gz);
        for (rel, is_dir, data) in &entries {
            let rel = rel.to_string_lossy().replace('\\', "/");
            let mut header = tar::Header::new_gnu();
            if *is_dir {
                header.set_entry_type(tar::EntryType::dir());
                header.set_size(0);
                header.set_mode(0o755);
            } else {
                header.set_entry_type(tar::EntryType::file());
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
            }
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            // `tar::Header::new_gnu()` already has empty uname/gname, which is
            // the normalized value we want for a reproducible archive.
            header.set_cksum();
            builder
                .append_data(&mut header, &rel, data.as_slice())
                .context("failed to append entry to tar")?;
        }
        builder.finish().context("failed to finish tar")?;
    }
    let gz_bytes = gz.finish().context("failed to finish gzip")?;
    anyhow::ensure!(
        gz_bytes.len() <= MAX_UI_GZ_BYTES,
        "web UI gzip size {} exceeds the {MAX_UI_GZ_BYTES} limit",
        gz_bytes.len()
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").context("OUT_DIR not set")?);
    fs::write(out_dir.join("web_ui_bundle.gz"), &gz_bytes)
        .context("failed to write web_ui_bundle.gz")?;

    let bundle_rs = format!(
        "// Generated by build.rs. Do not edit.\n\
         /// Embedded, gzip-compressed tarball of the web UI (`dist/`).\n\
         pub const WEB_UI_BUNDLE_GZ: &[u8] =\n\
         \x20\x20\x20\x20include_bytes!(concat!(env!(\"OUT_DIR\"), \"/web_ui_bundle.gz\"));\n\
         /// Number of files in the embedded bundle.\n\
         pub const WEB_UI_FILE_COUNT: usize = {file_count};\n\
         /// Uncompressed size of the embedded bundle in bytes.\n\
         pub const WEB_UI_UNCOMPRESSED_BYTES: usize = {uncompressed};\n"
    );
    fs::write(out_dir.join("web_ui_bundle.rs"), bundle_rs)
        .context("failed to write web_ui_bundle.rs")?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=linker.ld");
    let out_dir = PathBuf::from(env::var("OUT_DIR").context("OUT_DIR is not set")?);
    let linker = out_dir.join("linker.x");
    fs::write(&linker, include_str!("linker.ld"))?;
    println!("cargo:rustc-link-search={}", out_dir.display());
    fs::write(
        out_dir.join("../../..").join("linker.x"),
        include_str!("linker.ld"),
    )?;

    let arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").context("CARGO_CFG_TARGET_ARCH is not set")?;

    let platform = fallback_platform_for_arch(&arch);

    println!("cargo:rustc-cfg=platform=\"{platform}\"");

    let config_paths = get_config_paths().unwrap_or_default();
    let config_files = get_configs();
    let mut output_file = open_output_file();

    println!("cargo:rerun-if-env-changed=AXVISOR_VM_CONFIGS");
    println!("cargo:rerun-if-changed=build.rs");
    for path in &config_paths {
        println!(
            "cargo:rerun-if-changed={}",
            PathBuf::from(path.clone()).display()
        );
    }

    match config_files {
        Ok(config_files) => {
            let output = if config_files.is_empty() {
                quote! {
                    pub fn static_vm_configs() -> Vec<&'static str> {
                        default_static_vm_configs()
                    }
                }
            } else {
                let configs = config_files.iter().map(|config_file| {
                    LitStr::new(&config_file.content, proc_macro2::Span::call_site())
                });
                quote! {
                    pub fn static_vm_configs() -> Vec<&'static str> {
                        vec![#(#configs),*]
                    }
                }
            };
            write_tokens(&mut output_file, output)?;

            // generate "load kernel and dtb images function"
            generate_firmware_img_loading_functions(&mut output_file, &config_files)?;
            generate_guest_img_loading_functions(&mut output_file, config_files)?;
        }
        Err(error) => {
            let error = LitStr::new(&error, proc_macro2::Span::call_site());
            let output = quote! {
                pub fn static_vm_configs() -> Vec<&'static str> {
                    compile_error!(#error)
                }
            };
            write_tokens(&mut output_file, output)?;
        }
    }

    // Build and embed the web UI when the feature is enabled. A build failure
    // surfaces as a `compile_error!` through the generated `web_ui_bundle.rs`
    // so the operator sees the exact cause instead of an opaque include error.
    #[cfg(feature = "web-ui")]
    {
        if let Err(e) = build_web_ui_bundle() {
            let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap_or_else(|_| "OUT_DIR".into()));
            let _ = fs::write(
                out_dir.join("web_ui_bundle.rs"),
                format!("compile_error!(\"web-ui build failed: {e}\");"),
            );
        }
        println!("cargo:rerun-if-changed=web-ui/package.json");
        println!("cargo:rerun-if-changed=web-ui/package-lock.json");
        println!("cargo:rerun-if-changed=web-ui/src");
        println!("cargo:rerun-if-changed=web-ui/index.html");
        println!("cargo:rerun-if-changed=web-ui/vite.config.ts");
    }

    Ok(())
}
