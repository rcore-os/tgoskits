//! This build script reads config file paths from the `UMHV_VM_CONFIGS` environment variable,
//! reads them, and then outputs them to `$(OUT_DIR)/vm_configs.rs` to be used by
//! `src/vmm/config.rs`.
//!
//! The `UMHV_VM_CONFIGS` environment variable should follow the format convention for the
//! `PATH` environment variable on the building platform, i.e., paths are separated by colons
//! (`:`) on Unix-like systems and semicolons (`;`) on Windows.
//!
//! In the generated `vm_configs.rs` file, a function `static_vm_configs` is defined that
//! returns a `Vec<&'static str>` containing the contents of the configuration files.
//!
//! If the `UMHV_VM_CONFIGS` environment variable is not set, `static_vm_configs` will call
//! the `default_static_vm_configs` function from `src/vmm/config.rs` to return the default
//! configurations.
//!
//! If the `UMHV_VM_CONFIGS` environment variable is set but the configuration files cannot be
//! read, the build script will output a `compile_error!` macro that will cause the build to
//! fail.
//!
//! This build script reruns if the `UMHV_VM_CONFIGS` environment variable changes, or if the
//! `build.rs` file changes, or if any of the files in the paths specified by `UMHV_VM_CONFIGS`
//! change.
use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use toml::Value;

static CONFIGS_DIR_PATH: &str = "configs";

/// A configuration file that has been read from disk.
struct ConfigFile {
    /// The path to the configuration file.
    pub path: OsString,
    /// The contents of the configuration file.
    pub content: String,
}

/// Gets the paths (colon-separated) from the `UMHV_VM_CONFIGS` environment variable.
///
/// Returns `None` if the environment variable is not set.
fn get_config_paths() -> Option<Vec<OsString>> {
    env::var_os("UMHV_VM_CONFIGS")
        .map(|paths| env::split_paths(&paths).map(OsString::from).collect())
}

/// Gets the paths and contents of the configuration files specified by the `UMHV_VM_CONFIGS` environment variable.
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

/// Generate function to load guest images from config
/// Toml file must be provided to load from memory.
/// Only load the first config item, if there are multiple items in the config file.
/// Other VMs are dynamically loaded from the file system by the first VM that starts.
fn generate_guest_img_loading_functions(
    out_file: &mut fs::File,
    config_files: Vec<ConfigFile>,
) -> io::Result<()> {
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

    let mut find_memory_image = false;

    for config_file in config_files {
        let config = config_file
            .content
            .parse::<Value>()
            .expect("failed to parse config file");
        if let Some(image_location_val) = config.get("image_location") {
            if let Some(image_location) = image_location_val.as_str() {
                if image_location == "memory" {
                    // Check if there are multiple VMs in the config file list to be loaded from memory.
                    // Cause we only support one VM to be loaded from memory at most.
                    if find_memory_image {
                        writeln!(
                            out_file,
                            r#"pub fn error_msg() -> Option<&'static [u8]> {{ "#
                        )?;
                        writeln!(out_file, "    compile_error!(\"ArceOS-Hypervisor currently only supports loading one guestVM image from memory\")")?;
                        writeln!(out_file, "}}\n")?;
                        break;
                    } else {
                        find_memory_image = true;
                    }

                    writeln!(
                        out_file,
                        r#"pub fn get_kernel_binary() -> Option<&'static [u8]> {{ "#
                    )?;
                    if let Some(kernel_path) = config.get("kernel_path") {
                        let kernel_path =
                            convert_to_absolute(CONFIGS_DIR_PATH, kernel_path.as_str().unwrap());
                        // use include_bytes! load image
                        writeln!(out_file, "    Some(include_bytes!({:?}))", kernel_path)?;
                    } else {
                        writeln!(out_file, "    compile_error!(\"Kernel image path is not provided if you want to compile the binary file together!\")"
                            )?;
                    };

                    writeln!(out_file, "}}\n")?;

                    writeln!(
                        out_file,
                        r#"pub fn get_dtb_binary() -> Option<&'static [u8]> {{ "#
                    )?;
                    if let Some(dtb_path) = config.get("dtb_path") {
                        let dtb_path =
                            convert_to_absolute(CONFIGS_DIR_PATH, dtb_path.as_str().unwrap());
                        // use include_bytes! load image
                        writeln!(out_file, "    Some(include_bytes!({:?}))", dtb_path)?;
                    } else {
                        writeln!(out_file, "    None")?;
                    };
                    writeln!(out_file, "}}\n")?;

                    writeln!(
                        out_file,
                        r#"pub fn get_bios_binary() -> Option<&'static [u8]> {{ "#
                    )?;
                    if let Some(bios_path) = config.get("bios_path") {
                        let bios_path =
                            convert_to_absolute(CONFIGS_DIR_PATH, bios_path.as_str().unwrap());
                        // use include_bytes! load image
                        writeln!(out_file, "    Some(include_bytes!({:?}))", bios_path)?;
                    } else {
                        writeln!(out_file, "    None")?;
                    };

                    writeln!(out_file, "}}\n")?;
                }
            }
        }
    }

    if !find_memory_image {
        writeln!(
            out_file,
            r#"pub fn get_kernel_binary() -> Option<&'static [u8]> {{ "#
        )?;
        writeln!(out_file, "    None")?;
        writeln!(out_file, "}}\n")?;
        writeln!(
            out_file,
            r#"pub fn get_dtb_binary() -> Option<&'static [u8]> {{ "#
        )?;
        writeln!(out_file, "    None")?;
        writeln!(out_file, "}}\n")?;
        writeln!(
            out_file,
            r#"pub fn get_bios_binary() -> Option<&'static [u8]> {{ "#
        )?;
        writeln!(out_file, "    None")?;
        writeln!(out_file, "}}")?;
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let platform = env::var("AX_PLATFORM").unwrap_or("".to_string());
    let platform_family = env::var("AX_PLATFORM").unwrap_or("".to_string());
    println!("cargo:rustc-cfg=platform=\"{}\"", platform);
    println!("cargo:rustc-cfg=platform_family=\"{}\"", platform_family);

    let config_files = get_configs();
    let mut output_file = open_output_file();

    println!("cargo:rerun-if-env-changed=UMHV_VM_CONFIGS");
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
