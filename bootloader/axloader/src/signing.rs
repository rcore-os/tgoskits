use std::{fs, io::Write, path::PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign a final ELF image for an axloader with the corresponding public key.
    Sign {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_parser = ["httpboot_entry"])]
        entry_symbol: Option<String>,
    },
}

pub fn run() -> anyhow::Result<()> {
    let Command::Sign {
        key,
        input,
        output,
        entry_symbol,
    } = Args::parse().command;
    let pem =
        zeroize::Zeroizing::new(fs::read_to_string(&key).context("failed to read signing key")?);
    let mut image =
        fs::read(&input).with_context(|| format!("failed to read {}", input.display()))?;
    let public_key =
        axloader::authentication::sign_image(&mut image, &pem, entry_symbol.as_deref())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| {
            format!(
                "failed to create {} (output must not exist)",
                output.display()
            )
        })?;
    file.write_all(&image)
        .with_context(|| format!("failed to write {}", output.display()))?;
    println!("{public_key}");
    Ok(())
}
