use std::io;
use std::path::Path;

use clap::{builder::PossibleValuesParser, Parser};
use toml_edit::DocumentMut;

/// Create a new platform package
#[derive(Parser, Debug)]
#[command(long_about = "Create a new platform package at <PATH> from the template")]
pub struct CommandNew {
    #[arg(required = true)]
    path: String,

    /// Set the CPU architecture for the platform
    #[arg(long, default_value = "x86_64")]
    arch: String,

    /// Set the platform name, defaults to the directory name
    #[arg(long)]
    name: Option<String>,

    /// Edition to set for the crate generated
    #[arg(
        long,
        value_name = "YEAR",
        value_parser = PossibleValuesParser::new(["2015", "2018", "2021", "2024"]),
    )]
    edition: Option<String>,
}

fn apply_cargo_toml_template(orig: &mut DocumentMut, new: &DocumentMut) {
    orig["dependencies"] = new["dependencies"].clone();
}

fn apply_template(path: &str, arch: &str) -> io::Result<()> {
    let path = Path::new(path);

    let cargo_toml = std::fs::read_to_string(path.join("Cargo.toml"))?;
    let mut orig_table = cargo_toml.parse::<DocumentMut>().unwrap();
    let package = orig_table["package"].as_table().unwrap();
    let plat_name = String::from(package["name"].as_str().unwrap());

    for (name, content) in crate::template::TEMPLATE {
        let dst = path.join(name);
        match *name {
            "Cargo.toml" => {
                let new_table = content.parse::<DocumentMut>().unwrap();
                apply_cargo_toml_template(&mut orig_table, &new_table);
                std::fs::write(dst, orig_table.to_string())?;
            }
            "axconfig.toml" => {
                let content = content
                    .replace("<ARCH>", arch)
                    .replace("<PLATFORM>", &plat_name);
                std::fs::write(dst, content)?;
            }
            _ => std::fs::write(dst, content)?,
        }
    }
    Ok(())
}

pub fn new_platform(args: CommandNew) {
    crate::run_cargo_command("new", |cmd| {
        cmd.arg("--lib").arg(&args.path);
        if let Some(name) = args.name {
            cmd.arg("--name").arg(name);
        }
        if let Some(edition) = args.edition {
            cmd.arg("--edition").arg(edition);
        }
    });

    apply_template(&args.path, &args.arch).unwrap();
}
