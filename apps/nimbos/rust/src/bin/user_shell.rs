#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

const LF: u8 = b'\n';
const CR: u8 = b'\r';
const DL: u8 = b'\x7f';
const BS: u8 = b'\x08';

use user_lib::console::getchar;
use user_lib::{exec, fork, waitpid};

const MAX_CMD_LEN: usize = 256;

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    println!("Rust user shell");
    let mut line = [0; MAX_CMD_LEN];
    let mut cursor = 0;
    print!(">> ");
    loop {
        let c = getchar();
        match c {
            LF | CR => {
                println!();
                if cursor > 0 {
                    line[cursor] = b'\0';
                    let pid = fork();
                    if pid == 0 {
                        // child process
                        let path = core::str::from_utf8(&line[..cursor]).unwrap();
                        if exec(path) < 0 {
                            println!("command not found: {:?}", path);
                            return -4;
                        }
                        unreachable!();
                    } else {
                        let mut exit_code = 0;
                        let exit_pid = waitpid(pid, Some(&mut exit_code), 0);
                        assert_eq!(pid, exit_pid);
                        println!("Shell: Process {} exited with code {}", pid, exit_code);
                    }
                    cursor = 0;
                }
                print!(">> ");
            }
            BS | DL => {
                if cursor > 0 {
                    print!("{}", BS as char);
                    print!(" ");
                    print!("{}", BS as char);
                    cursor -= 1;
                }
            }
            _ => {
                print!("{}", c as char);
                line[cursor] = c;
                cursor += 1;
            }
        }
    }
}
