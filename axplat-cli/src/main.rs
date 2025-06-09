use std::io::Write;
use std::process::Command;

use clap::{Parser, Subcommand};

mod add;
mod new;

#[rustfmt::skip]
mod template;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    New(self::new::CommandNew),
    Add(self::add::CommandAdd),
}

fn run_cargo_command(command: &str, add_args: impl FnOnce(&mut Command)) -> String {
    let mut cmd = Command::new("cargo");
    cmd.arg(command).arg("--color").arg("always");

    add_args(&mut cmd);

    let output = cmd.output().expect("failed to execute `cargo add`");
    std::io::stdout().write_all(&output.stdout).unwrap();
    std::io::stderr().write_all(&output.stderr).unwrap();
    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }
    String::from_utf8(output.stdout).unwrap()
}

fn main() {
    match Args::parse().command {
        Commands::New(args) => {
            self::new::new_platform(args);
        }
        Commands::Add(args) => {
            self::add::add_platform(args);
        }
    }
}
