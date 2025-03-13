//! This build script reads config file paths from the `AXVISOR_VM_CONFIGS` environment variable,
//! reads them, and then outputs them to `$(OUT_DIR)/vm_configs.rs` to be used by
//! `src/vmm/config.rs`.
//!
//! The `AXVISOR_VM_CONFIGS` environment variable should follow the format convention for the `PATH`
//! environment variable on the building platform, i.e., paths are separated by colons (`:`) on
//! Unix-like systems and semicolons (`;`) on Windows.
//!
//! In the generated `vm_configs.rs` file, a function `static_vm_configs` is defined that returns a
//! `Vec<&'static str>` containing the contents of the configuration files.
//!
//! If the `AXVISOR_VM_CONFIGS` environment variable is not set, `static_vm_configs` will call the
//! `default_static_vm_configs` function from `src/vmm/config.rs` to return the default
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
    io::{self, Write},
    path::{Path, PathBuf},
};

use quote::quote;
use toml::Value;

static CONFIGS_DIR_PATH: &str = "configs/vms";

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
    env::var_os("AXVISOR_VM_CONFIGS")
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
    let output_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let output_file = output_dir.join("vm_configs.rs");

    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output_file)
        .unwrap()
}

// Convert relative path to absolute path
fn convert_to_absolute(configs_path: &str, path: &str) -> PathBuf {
    let path = Path::new(path);
    let configs_path = Path::new(configs_path).join(path);
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
}

fn parse_config_file(config_file: &ConfigFile) -> Option<MemoryImage> {
    let config = config_file
        .content
        .parse::<Value>()
        .expect("failed to parse config file");

    let id = config.get("base")?.as_table()?.get("id")?.as_integer()? as usize;

    let image_location_val = config.get("kernel")?.as_table()?.get("image_location")?;

    let image_location = image_location_val.as_str()?;

    if image_location != "memory" {
        return None;
    }

    let kernel_path = config.get("kernel")?.as_table()?.get("kernel_path")?;

    let kernel = convert_to_absolute(CONFIGS_DIR_PATH, kernel_path.as_str().unwrap());

    let dtb = config
        .get("kernel")?
        .as_table()?
        .get("dtb_path")
        .and_then(|v| v.as_str())
        .map(|v| convert_to_absolute(CONFIGS_DIR_PATH, v));

    let bios = config
        .get("kernel")?
        .as_table()?
        .get("bios_path")
        .and_then(|v| v.as_str())
        .map(|v| convert_to_absolute(CONFIGS_DIR_PATH, v));

    Some(MemoryImage {
        id,
        kernel,
        dtb,
        bios,
    })
}

/// Generate function to load guest images from config
/// Toml file must be provided to load from memory.
fn generate_guest_img_loading_functions(
    out_file: &mut fs::File,
    config_files: Vec<ConfigFile>,
) -> io::Result<()> {
    let mut memory_images = vec![];

    for config_file in config_files {
        if let Some(files) = parse_config_file(&config_file) {
            let id = files.id;
            let kernel = files.kernel.display().to_string();
            let dtb = match files.dtb {
                Some(v) => {
                    let s = v.display().to_string();
                    quote! { Some(include_bytes!(#s)) }
                }
                None => quote! { None },
            };

            let bios = match files.bios {
                Some(v) => {
                    let s = v.display().to_string();
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
        }

        /// Get memory images from config file.
        pub fn get_memory_images() -> &'static [MemoryImage] {
            &[
                #(#memory_images),*
            ]
        }
    };
    let syntax_tree = syn::parse2(output).unwrap();
    let formatted = prettyplease::unparse(&syntax_tree);
    out_file.write_all(formatted.as_bytes())?;

    Ok(())
}

fn main() -> io::Result<()> {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    let platform = env::var("AX_PLATFORM").unwrap_or("".to_string());
    println!("cargo:rustc-cfg=platform=\"{}\"", platform);

    if platform != "dummy" {
        gen_linker_script(&arch, platform.as_str()).unwrap();
    }

    let config_files = get_configs();
    let mut output_file = open_output_file();

    println!("cargo:rerun-if-env-changed=AXVISOR_VM_CONFIGS");
    println!("cargo:rerun-if-changed=build.rs");

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
                    writeln!(output_file, "        r###\"{}\"###,", config_file.content)?;
                    println!(
                        "cargo:rerun-if-changed={}",
                        PathBuf::from(config_file.path.clone()).display()
                    );
                }
                writeln!(output_file, "    ]")?;
            }
            writeln!(output_file, "}}\n")?;

            // generate "load kernel and dtb images function"
            generate_guest_img_loading_functions(&mut output_file, config_files)?;
        }
        Err(error) => {
            writeln!(output_file, "    compile_error!(\"{}\")", error)?;
            writeln!(output_file, "}}\n")?;
        }
    }
    Ok(())
}

fn gen_linker_script(arch: &str, platform: &str) -> io::Result<()> {
    let fname = format!("linker_{}.lds", platform);
    let output_arch = if arch == "x86_64" {
        "i386:x86-64"
    } else if arch.contains("riscv") {
        "riscv" // OUTPUT_ARCH of both riscv32/riscv64 is "riscv"
    } else {
        arch
    };
    let ld_content = std::fs::read_to_string("scripts/lds/linker.lds.S")?;
    let ld_content = ld_content.replace("%ARCH%", output_arch);
    let ld_content = ld_content.replace(
        "%KERNEL_BASE%",
        &format!("{:#x}", axconfig::plat::KERNEL_BASE_VADDR),
    );
    let ld_content = ld_content.replace("%SMP%", &format!("{}", axconfig::SMP));

    // target/<target_triple>/<mode>/build/axvisor-xxxx/out
    let out_dir = std::env::var("OUT_DIR").unwrap();
    // target/<target_triple>/<mode>/linker_xxxx.lds
    let out_path = Path::new(&out_dir).join("../../../").join(fname);

    println!("writing linker script to {}", out_path.display());
    std::fs::write(out_path, ld_content)?;
    Ok(())
}
