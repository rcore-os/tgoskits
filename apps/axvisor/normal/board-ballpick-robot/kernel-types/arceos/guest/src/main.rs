//! Basic ArceOS VM shell, adapted from apps/arceos/shell without disk commands.

use std::io::{self, Read, Write};

#[cfg(feature = "arceos")]
use ax_std as _;

const COMMANDS: &[&str] = &["help", "uname", "exit"];
const MAX_CMD_LEN: usize = 256;

fn main() {
    if let Err(error) = shell() {
        eprintln!("Shell I/O error: {error}");
        std::process::exit(1);
    }
}

fn shell() -> io::Result<()> {
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut line = Vec::with_capacity(MAX_CMD_LEN);
    let mut overflow = false;
    loop {
        print!("arceos:/$ ");
        stdout.flush()?;
        loop {
            let mut byte = [0];
            stdin.read_exact(&mut byte)?;
            match byte[0] {
                b'\r' | b'\n' => break,
                8 | 127 if !overflow => {
                    if line.pop().is_some() {
                        stdout.write_all(b"\x08 \x08")?;
                    }
                }
                0..=31 | 127 => {}
                c if !overflow && line.len() < MAX_CMD_LEN => {
                    line.push(c);
                    stdout.write_all(&[c])?;
                }
                _ => overflow = true,
            }
            stdout.flush()?;
        }
        println!();
        if overflow {
            println!("Command too long");
        } else {
            match std::str::from_utf8(&line) {
                Ok(command) => run_command(command.trim()),
                Err(_) => println!("Invalid UTF-8 command"),
            }
        }
        line.clear();
        overflow = false;
    }
}

fn run_command(command: &str) {
    match command {
        "help" => {
            println!("Available commands:");
            for name in COMMANDS {
                println!("  {name}");
            }
        }
        "uname" => {
            // Like arceos-shell, this is the application build version, not a Git SHA.
            let version = env!("CARGO_PKG_VERSION");
            let arch = std::env::consts::ARCH;
            let platform = option_env!("AX_PLATFORM").unwrap_or("");
            println!("ArceOS {version} {arch} {platform}");
        }
        "exit" => {
            println!("Bye~");
            std::process::exit(0);
        }
        "" => {}
        _ => println!("Unknown command: {command}"),
    }
}
