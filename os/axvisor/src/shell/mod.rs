// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod command;
mod connection;

use std::io::prelude::*;
use std::println;
use std::string::ToString;
use std::thread;
use std::time::Duration;

use crate::shell::command::{
    CommandHistory, clear_line_and_redraw, handle_builtin_commands, print_prompt, prompt_string,
    run_cmd_bytes,
};
#[cfg(target_arch = "aarch64")]
use crate::shell::connection::split_console_input;
use crate::shell::connection::{
    CONSOLE_INPUT_READ_SIZE, ConnectError, ConnectionToken, ConsoleConnectionState, DetachEvent,
};

const LF: u8 = b'\n';
const CR: u8 = b'\r';
const DL: u8 = b'\x7f';
const BS: u8 = b'\x08';
const ESC: u8 = 0x1b; // ESC key

const MAX_LINE_LEN: usize = 256;

static CONNECTION: ConsoleConnectionState = ConsoleConnectionState::new();

enum InputState {
    Normal,
    Escape,
    EscapeSeq,
}

struct ShellLineEditor {
    history: CommandHistory,
    buf: [u8; MAX_LINE_LEN],
    cursor: usize,
    line_len: usize,
    input_state: InputState,
}

impl ShellLineEditor {
    fn new(history_capacity: usize) -> Self {
        Self {
            history: CommandHistory::new(history_capacity),
            buf: [0; MAX_LINE_LEN],
            cursor: 0,
            line_len: 0,
            input_state: InputState::Normal,
        }
    }

    fn reset_input_state(&mut self) {
        self.input_state = InputState::Normal;
    }

    fn clear_current_line(&mut self) {
        self.buf[..self.line_len].fill(0);
        self.cursor = 0;
        self.line_len = 0;
    }

    fn redraw_current_line(&self, stdout: &mut std::io::Stdout) {
        let current = std::str::from_utf8(&self.buf[..self.line_len]).unwrap_or("");
        let prompt = prompt_string();
        clear_line_and_redraw(stdout, &prompt, current, self.cursor);
    }

    fn load_history_line(&mut self, line: &str) {
        self.clear_current_line();
        let bytes = line.as_bytes();
        let copy_len = bytes.len().min(MAX_LINE_LEN - 1);
        self.buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
        self.cursor = copy_len;
        self.line_len = copy_len;
    }

    fn submit_current_line(&mut self) {
        println!();
        if self.line_len > 0 {
            let command = std::str::from_utf8(&self.buf[..self.line_len])
                .unwrap_or("")
                .to_string();
            self.history.add_command(command.clone());
            if !handle_builtin_commands(&command) {
                run_cmd_bytes(command.as_bytes());
            }
            self.clear_current_line();
        }

        let Some(vm_id) = crate::shell::command::take_connect_request() else {
            print_prompt();
            return;
        };
        match CONNECTION.connect(vm_id) {
            Ok(_) => println!("[connected to VM[{vm_id}] console, press Ctrl+] to return]"),
            Err(ConnectError::AlreadyConnected) => {
                println!("Error: another VM console is already connected");
                print_prompt();
            }
            Err(ConnectError::VmIdOutOfRange) => {
                println!("Error: VM ID cannot be represented by the console connection state");
                print_prompt();
            }
        }
    }

    fn handle_normal_byte(&mut self, ch: u8, stdout: &mut std::io::Stdout) {
        match ch {
            CR | LF => self.submit_current_line(),
            BS | DL => {
                if self.cursor > 0 {
                    for i in self.cursor..self.line_len {
                        self.buf[i - 1] = self.buf[i];
                    }
                    self.cursor -= 1;
                    self.line_len -= 1;
                    self.buf[self.line_len] = 0;
                    self.redraw_current_line(stdout);
                }
            }
            ESC => self.input_state = InputState::Escape,
            0..=31 => {}
            byte => {
                if self.line_len < MAX_LINE_LEN - 1 {
                    for i in (self.cursor..self.line_len).rev() {
                        self.buf[i + 1] = self.buf[i];
                    }
                    self.buf[self.cursor] = byte;
                    self.cursor += 1;
                    self.line_len += 1;
                    self.redraw_current_line(stdout);
                }
            }
        }
    }

    fn handle_escape_sequence(&mut self, ch: u8, stdout: &mut std::io::Stdout) {
        match ch {
            b'A' => {
                if let Some(command) = self.history.previous().map(ToString::to_string) {
                    self.load_history_line(&command);
                    let prompt = prompt_string();
                    clear_line_and_redraw(stdout, &prompt, &command, self.cursor);
                }
            }
            b'B' => match self.history.next().map(ToString::to_string) {
                Some(command) => {
                    self.load_history_line(&command);
                    let prompt = prompt_string();
                    clear_line_and_redraw(stdout, &prompt, &command, self.cursor);
                }
                None => {
                    self.clear_current_line();
                    let prompt = prompt_string();
                    clear_line_and_redraw(stdout, &prompt, "", self.cursor);
                }
            },
            b'C' => {
                if self.cursor < self.line_len {
                    self.cursor += 1;
                    stdout.write_all(b"\x1b[C").ok();
                    stdout.flush().ok();
                }
            }
            b'D' => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    stdout.write_all(b"\x1b[D").ok();
                    stdout.flush().ok();
                }
            }
            b'3' => {}
            _ => {}
        }
        self.reset_input_state();
    }

    fn handle_byte(&mut self, ch: u8, stdout: &mut std::io::Stdout) {
        match self.input_state {
            InputState::Normal => self.handle_normal_byte(ch, stdout),
            InputState::Escape => {
                self.input_state = if ch == b'[' {
                    InputState::EscapeSeq
                } else {
                    InputState::Normal
                };
            }
            InputState::EscapeSeq => self.handle_escape_sequence(ch, stdout),
        }
    }
}

fn print_detached(vm_id: usize) {
    println!("\n[returned from VM[{vm_id}] console]");
    print_prompt();
}

fn detach_connection(snapshot: ConnectionToken) -> bool {
    match CONNECTION.detach(snapshot) {
        Some(DetachEvent::Detached { vm_id }) => {
            print_detached(vm_id);
            true
        }
        None => false,
    }
}

#[cfg(target_arch = "aarch64")]
fn pump_console_output(snapshot: ConnectionToken, stdout: &mut std::io::Stdout) {
    if CONNECTION.current() != Some(snapshot) {
        return;
    }
    let Some(vm) = crate::manager::AxvmManager::vm_by_id(snapshot.vm_id) else {
        detach_connection(snapshot);
        return;
    };
    if vm.status() != axvm::VmStatus::Running {
        if CONNECTION.current() == Some(snapshot) {
            detach_connection(snapshot);
        }
        return;
    }

    let mut output = [0u8; 256];
    loop {
        if CONNECTION.current() != Some(snapshot) {
            return;
        }
        let read = match vm.drain_console_output(&mut output) {
            Ok(read) => read,
            Err(_) => {
                if CONNECTION.current() == Some(snapshot) {
                    detach_connection(snapshot);
                }
                return;
            }
        };
        if read == 0 {
            return;
        }
        if CONNECTION.current() != Some(snapshot) {
            return;
        }
        stdout.write_all(&output[..read]).ok();
        stdout.flush().ok();
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn pump_console_output(snapshot: ConnectionToken, _stdout: &mut std::io::Stdout) {
    detach_connection(snapshot);
}

fn start_console_output_pump() {
    thread::spawn(|| {
        let mut stdout = std::io::stdout();
        loop {
            if let Some(snapshot) = CONNECTION.current() {
                pump_console_output(snapshot, &mut stdout);
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
}

#[cfg(target_arch = "aarch64")]
fn handle_connected_console_input(
    snapshot: ConnectionToken,
    input: &[u8],
    editor: &mut ShellLineEditor,
) {
    let (forward, detach_requested) = split_console_input(input);
    if CONNECTION.current() != Some(snapshot) {
        return;
    }
    let Some(vm) = crate::manager::AxvmManager::vm_by_id(snapshot.vm_id) else {
        if detach_connection(snapshot) {
            editor.reset_input_state();
        }
        return;
    };
    if vm.status() != axvm::VmStatus::Running {
        if CONNECTION.current() == Some(snapshot) && detach_connection(snapshot) {
            editor.reset_input_state();
        }
        return;
    }

    if !forward.is_empty() {
        if CONNECTION.current() != Some(snapshot) {
            return;
        }
        if vm.push_console_input(forward).is_err() {
            if CONNECTION.current() == Some(snapshot) && detach_connection(snapshot) {
                editor.reset_input_state();
            }
            return;
        }
    }

    if detach_requested && CONNECTION.current() == Some(snapshot) && detach_connection(snapshot) {
        editor.reset_input_state();
    }
}

#[cfg(not(target_arch = "aarch64"))]
fn handle_connected_console_input(
    snapshot: ConnectionToken,
    _input: &[u8],
    editor: &mut ShellLineEditor,
) {
    if detach_connection(snapshot) {
        editor.reset_input_state();
    }
}

// Initialize the console shell.
pub fn console_init() {
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut editor = ShellLineEditor::new(100);

    println!("Welcome to AxVisor Shell!");
    println!("Type 'help' to see available commands");
    println!("Use UP/DOWN arrows to navigate command history");
    #[cfg(not(feature = "fs"))]
    println!("Note: Running with limited features (filesystem support disabled).");
    println!();

    start_console_output_pump();
    print_prompt();

    loop {
        let mut input = [0u8; CONSOLE_INPUT_READ_SIZE];
        let input_len = match stdin.read(&mut input) {
            Ok(len) if len > 0 => len,
            _ => continue,
        };
        let mut consumed = 0;
        while consumed < input_len {
            if let Some(snapshot) = CONNECTION.current() {
                handle_connected_console_input(snapshot, &input[consumed..input_len], &mut editor);
                break;
            }
            editor.handle_byte(input[consumed], &mut stdout);
            consumed += 1;
        }
    }
}
