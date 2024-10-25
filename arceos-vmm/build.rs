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
    path::PathBuf,
};

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

fn main() -> io::Result<()> {
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
                for config_file in config_files {
                    writeln!(output_file, "        r###\"{}\"###,", config_file.content)?;
                    println!(
                        "cargo:rerun-if-changed={}",
                        PathBuf::from(config_file.path).display()
                    );
                }
                writeln!(output_file, "    ]")?;
            }
        }
        Err(error) => {
            writeln!(output_file, "    compile_error!(\"{}\")", error)?;
        }
    }
    writeln!(output_file, "}}")?;
    Ok(())
}
