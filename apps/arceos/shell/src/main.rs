#[cfg(feature = "arceos")]
use ax_std as _;

mod cmd;

use std::io::prelude::*;

const LF: u8 = b'\n';
const CR: u8 = b'\r';
const DL: u8 = b'\x7f';
const BS: u8 = b'\x08';
const SPACE: u8 = b' ';

const MAX_CMD_LEN: usize = 256;

fn path_to_str(path: &impl AsRef<std::ffi::OsStr>) -> &str {
    path.as_ref().to_str().unwrap()
}

fn print_prompt() {
    print!("arceos> ");
    std::io::stdout().flush().unwrap();
}

fn print_ready() {
    #[cfg(feature = "arceos")]
    {
        let elapsed = ax_api::time::ax_monotonic_time();
        println!(
            "ARCEOS_READY elapsed={}.{:03}s",
            elapsed.as_secs(),
            elapsed.subsec_millis()
        );
    }

    #[cfg(not(feature = "arceos"))]
    println!("ARCEOS_READY");
}

fn main() {
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let mut buf = [0; MAX_CMD_LEN];
    let mut cursor = 0;
    print_ready();
    print_prompt();

    loop {
        if stdin.read(&mut buf[cursor..cursor + 1]).ok() != Some(1) {
            continue;
        }
        if buf[cursor] == b'\x1b' {
            buf[cursor] = b'^';
        }
        match buf[cursor] {
            CR | LF => {
                println!();
                if cursor > 0 {
                    cmd::run_cmd(&buf[..cursor]);
                    cursor = 0;
                }
                print_prompt();
            }
            BS | DL => {
                if cursor > 0 {
                    stdout.write_all(&[BS, SPACE, BS]).unwrap();
                    cursor -= 1;
                }
            }
            0..=31 => {}
            c => {
                if cursor < MAX_CMD_LEN - 1 {
                    stdout.write_all(&[c]).unwrap();
                    cursor += 1;
                }
            }
        }
    }
}
