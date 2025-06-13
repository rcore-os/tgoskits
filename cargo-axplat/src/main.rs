use std::io::Write;
use std::process::Command;

use clap::{Args, Parser, Subcommand};
use clap_cargo::style::CLAP_STYLING;

mod add;
mod info;
mod new;

#[rustfmt::skip]
mod template {
    include!(concat!(env!("OUT_DIR"), "/template.rs"));
}

#[derive(Parser, Debug)]
#[command(bin_name = "cargo", version, styles = CLAP_STYLING)]
enum Cargo {
    Axplat(CargoCommand),
}

#[derive(Args, Debug)]
#[command(arg_required_else_help = true)]
struct CargoCommand {
    /// Print version
    #[arg(short = 'V', long, global = true)]
    version: bool,

    #[command(subcommand)]
    command: Option<AxplatCommand>,
}

/// Manages hardware platform packages using `axplat`
#[derive(Subcommand, Debug)]
enum AxplatCommand {
    New(self::new::CommandNew),
    Add(self::add::CommandAdd),
    Info(self::info::CommandInfo),
}

fn run_cargo_command(command: &str, add_args: impl FnOnce(&mut Command)) -> String {
    let mut cmd = Command::new("cargo");
    cmd.arg(command).arg("--color").arg("always");

    add_args(&mut cmd);

    let output = cmd
        .output()
        .unwrap_or_else(|_| panic!("error: failed to execute `cargo {command}`"));
    std::io::stderr().write_all(&output.stderr).unwrap();
    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }
    String::from_utf8(output.stdout).unwrap()
}

fn main() {
    let Cargo::Axplat(axplat) = Cargo::parse();
    if axplat.version {
        println!("cargo-axplat {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    match axplat.command.unwrap() {
        AxplatCommand::New(args) => {
            self::new::new_platform(args);
        }
        AxplatCommand::Add(args) => {
            self::add::add_platform(args);
        }
        AxplatCommand::Info(args) => {
            self::info::platform_info(args);
        }
    }
}
