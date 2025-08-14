//! ArceOS-Hypervisor VM configuration tool
//!
//! This module provides a command-line interface for managing VM configurations,
//! including validation of existing configurations and generation of new templates.
use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;

use clap::{Args, Parser, Subcommand};

use crate::templates::get_vm_config_template;
use crate::AxVMCrateConfig;

/// Main CLI structure for the axvmconfig tool
///
/// This structure defines the top-level command interface using clap.
/// It supports subcommands for checking existing configurations and
/// generating new configuration templates.
#[derive(Parser)]
#[command(name = "axvmconfig")]
#[command(about = "A simple VM configuration tool for ArceOS-Hypervisor.", long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
#[command(flatten_help = true)]
pub struct CLI {
    #[command(subcommand)]
    pub subcmd: CLISubCmd,
}

/// Available subcommands for the CLI tool
///
/// Currently supports two main operations:
/// - Check: Validate existing TOML configuration files
/// - Generate: Create new configuration templates from command-line parameters
#[derive(Subcommand)]
#[command(args_conflicts_with_subcommands = true)]
#[command(flatten_help = true)]
pub enum CLISubCmd {
    /// Parse the configuration file and check its validity.
    Check(CheckArgs),
    /// Generate a template configuration file.
    Generate(TemplateArgs),
}

/// Arguments for the 'check' subcommand
///
/// Used to validate existing TOML configuration files for correctness.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Path to the TOML configuration file to validate
    #[arg(short, long)]
    config_path: String,
}

/// Arguments for the 'generate' subcommand
///
/// Used to create new VM configuration templates with customizable parameters.
/// All the essential VM settings can be specified through command-line arguments.
#[derive(Debug, Args)]
pub struct TemplateArgs {
    /// The architecture of the VM, currently only support "riscv64", "aarch64" and "x86_64".
    #[arg(short = 'a', long)]
    arch: String,
    /// The ID of the VM.
    #[arg(short = 'i', long, default_value_t = 0)]
    id: usize,
    /// The name of the VM.
    #[arg(short = 'n', long, default_value_t = String::from("GuestVM"))]
    name: String,
    /// The type of the VM, 0 for HostVM, 1 for RTOS, 2 for Linux.
    #[arg(short = 't', long, default_value_t = 1)]
    vm_type: usize,
    /// The number of CPUs of the VM.
    #[arg(short = 'c', long, default_value_t = 1)]
    cpu_num: usize,
    /// The entry point of the VM.
    #[arg(short = 'e', long, default_value_t = 1)]
    entry_point: usize,
    /// The path of the kernel image, if the image_location is "fs", it should be the path of the kernel image file inside the ArceOS's rootfs.
    #[arg(short = 'k', long)]
    kernel_path: String,
    /// The load address of the kernel image.
    #[arg(short = 'l', long, value_parser = parse_usize)]
    kernel_load_addr: usize,
    /// The location of the kernel image：
    /// - "fs" for the kernel image file inside the ArceOS's rootfs
    /// - "memory" for the kernel image file in the memory.
    #[arg(long, default_value_t = String::from("fs"))]
    image_location: String,
    /// The command line of the kernel.
    #[arg(long)]
    cmdline: Option<String>,
    /// The output path of the template file.
    #[arg(short = 'O', long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    output: Option<std::path::PathBuf>,
}

/// Parse numeric values from command line arguments
///
/// Supports multiple number formats:
/// - Hexadecimal (0x prefix): e.g., 0x80200000
/// - Binary (0b prefix): e.g., 0b10101010  
/// - Decimal: e.g., 123456
///
/// # Arguments
/// * `s` - String slice containing the number to parse
///
/// # Returns
/// * `Result<usize, Box<dyn Error + Send + Sync + 'static>>` - Parsed number or error
fn parse_usize(s: &str) -> Result<usize, Box<dyn Error + Send + Sync + 'static>> {
    if s.starts_with("0x") {
        // Parse hexadecimal number
        Ok(usize::from_str_radix(&s[2..], 16)?)
    } else if s.starts_with("0b") {
        // Parse binary number
        Ok(usize::from_str_radix(&s[2..], 2)?)
    } else {
        // Parse decimal number
        Ok(s.parse()?)
    }
}

/// Main entry point for the CLI tool
///
/// Parses command line arguments and dispatches to appropriate handlers
/// for either configuration validation or template generation.
pub fn run() {
    let cli = CLI::parse();
    match cli.subcmd {
        // Handle configuration file validation
        CLISubCmd::Check(args) => {
            let file_path = &args.config_path;

            // Check if the specified file exists
            if !Path::new(file_path).exists() {
                eprintln!("Error: File '{}' does not exist.", file_path);
                std::process::exit(1);
            }

            // Read the configuration file content
            let file_content = match fs::read_to_string(file_path) {
                Ok(content) => content,
                Err(err) => {
                    eprintln!("Error: Failed to read file '{}': {}", file_path, err);
                    std::process::exit(1);
                }
            };

            // Parse and validate the TOML configuration
            match AxVMCrateConfig::from_toml(&file_content) {
                Ok(config) => {
                    println!("Config file '{}' is valid.", file_path);
                    println!("Config: {:#x?}", config);
                }
                Err(err) => {
                    eprintln!("Error: Config file '{}' is invalid: {}", file_path, err);
                    std::process::exit(1);
                }
            }
        }
        // Handle template generation
        CLISubCmd::Generate(args) => {
            // Determine the kernel path based on image location
            // For memory-based images, use absolute path; for fs-based, use relative path
            let kernel_path = if args.image_location == "memory" {
                Path::new(&args.kernel_path)
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            } else {
                args.kernel_path.clone()
            };

            // Generate the VM configuration template with provided parameters
            let template = get_vm_config_template(
                args.id,
                args.name + "-" + args.arch.as_str(),
                args.vm_type,
                args.cpu_num,
                args.entry_point,
                kernel_path,
                args.kernel_load_addr,
                args.image_location,
                args.cmdline,
            );

            // Convert the configuration template to TOML format
            let template_toml = toml::to_string(&template).unwrap();

            // Determine output file path (use provided path or default to current directory)
            let target_path = match args.output {
                Some(relative_path) => relative_path,
                None => env::current_dir().unwrap().join("template.toml"),
            };

            // Write the generated template to file
            match fs::write(&target_path, template_toml) {
                Ok(_) => {
                    println!("Template file '{:?}' has been generated.", target_path);
                }
                Err(err) => {
                    eprintln!(
                        "Error: Failed to write template file '{:?}': {}",
                        target_path, err
                    );
                    std::process::exit(1);
                }
            }
        }
    }
}
