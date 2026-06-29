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
};

use anyhow::Context;
use quote::{ToTokens, quote};
use syn::LitStr;
use toml::Table;

fn fallback_platform_for_arch(arch: &str) -> &'static str {
    match arch {
        "aarch64" => "aarch64-generic",
        "loongarch64" => "loongarch64-qemu-virt",
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

// Convert relative path to absolute path
fn convert_to_absolute(configs_path: impl AsRef<Path>, path: &str) -> PathBuf {
    let path = Path::new(path);
    let configs_path = configs_path
        .as_ref()
        .parent()
        .map(|parent| parent.join(path))
        .unwrap_or_else(|| path.to_path_buf());
    if path.is_relative() {
        fs::canonicalize(configs_path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
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

    let kernel = convert_to_absolute(&config_file.path, kernel_path.as_str()?);

    let dtb = config
        .get("kernel")?
        .as_table()?
        .get("dtb_path")
        .and_then(|v| v.as_str())
        .map(|v| convert_to_absolute(&config_file.path, v));

    let enable_bios = config
        .get("kernel")?
        .as_table()?
        .get("enable_bios")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let kernel_config = config.get("kernel")?.as_table()?;

    let bios = boot_firmware_path(kernel_config, enable_bios)
        .map(|v| convert_to_absolute(&config_file.path, v));

    let ramdisk = kernel_config
        .get("ramdisk_path")
        .and_then(|v| v.as_str())
        .map(|v| convert_to_absolute(&config_file.path, v));

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
        .map(|v| convert_to_absolute(&config_file.path, v))?;

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
                MemoryImage {
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
        /// One guest image data from memory.
        pub struct MemoryImage{
            /// vm id in config file
            pub id: usize,
            /// kernel image
            pub kernel: &'static [u8],
            /// dtb image
            pub dtb: Option<&'static [u8]>,
            /// bios image
            pub bios: Option<&'static [u8]>,
            /// ramdisk image
            pub ramdisk: Option<&'static [u8]>,
        }

        /// Get memory images from config file.
        pub fn get_memory_images() -> &'static [MemoryImage] {
            &[
                #(#memory_images),*
            ]
        }
    };
    let syntax_tree = syn::parse2(output)?;
    let formatted = prettyplease::unparse(&syntax_tree);
    out_file.write_all(formatted.as_bytes())?;

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
                FirmwareImage {
                    id: #id,
                    bios: include_bytes!(#bios),
                }
            });
        }
    }

    let output = quote! {
        /// One guest firmware image loaded from the build host.
        pub struct FirmwareImage {
            /// vm id in config file
            pub id: usize,
            /// boot firmware image
            pub bios: &'static [u8],
        }

        /// Get firmware images from config file.
        pub fn get_firmware_images() -> &'static [FirmwareImage] {
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

    writeln!(
        output_file,
        "pub fn static_vm_configs() -> Vec<&'static str> {{"
    )?;

    match config_files {
        Ok(config_files) => {
            if config_files.is_empty() {
                writeln!(output_file, "    default_static_vm_configs()")?;
            } else {
                writeln!(output_file, "    vec![")?;
                for config_file in &config_files {
                    let content = LitStr::new(&config_file.content, proc_macro2::Span::call_site());
                    writeln!(output_file, "        {},", content.to_token_stream())?;
                }
                writeln!(output_file, "    ]")?;
            }
            writeln!(output_file, "}}\n")?;

            // generate "load kernel and dtb images function"
            generate_firmware_img_loading_functions(&mut output_file, &config_files)?;
            generate_guest_img_loading_functions(&mut output_file, config_files)?;
        }
        Err(error) => {
            writeln!(output_file, "    compile_error!(\"{error}\")")?;
            writeln!(output_file, "}}\n")?;
        }
    }
    Ok(())
}
