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

use std::io::prelude::*;
use std::string::ToString;

#[cfg(feature = "browser-console")]
use core::cell::Cell;

#[cfg(feature = "browser-console")]
std::thread_local! {
    static NETWORK_OUTPUT_SELECTED: Cell<bool> = const { Cell::new(false) };
}

fn submit_shell_fragment(args: core::fmt::Arguments<'_>) {
    let output = axvisor::shell_support::format_fragment(args);
    submit_shell_bytes(output.as_bytes());
}

fn submit_shell_line(args: core::fmt::Arguments<'_>) {
    let output = axvisor::shell_support::format_line(args);
    submit_shell_bytes(output.as_bytes());
}

pub(crate) fn submit_shell_bytes(bytes: &[u8]) {
    #[cfg(feature = "browser-console")]
    if NETWORK_OUTPUT_SELECTED.with(Cell::get) {
        crate::network_console::submit_management_output(bytes);
        return;
    }
    crate::guest_console::submit_host_bytes(bytes);
}

macro_rules! print {
    ($($arg:tt)*) => {
        crate::shell::submit_shell_fragment(format_args!($($arg)*))
    };
}

macro_rules! println {
    () => {
        crate::shell::submit_shell_line(format_args!(""))
    };
    ($($arg:tt)*) => {
        crate::shell::submit_shell_line(format_args!($($arg)*))
    };
}

mod command;

use crate::guest_console::ConsoleInputEvent;
use crate::shell::command::{
    CommandHistory, handle_builtin_commands, print_prompt, prompt_string, redraw_line,
    run_cmd_bytes,
};

const LF: u8 = b'\n';
const CR: u8 = b'\r';
const DL: u8 = b'\x7f';
const BS: u8 = b'\x08';
const ESC: u8 = 0x1b; // ESC key

const MAX_LINE_LEN: usize = 256;

enum InputState {
    Normal,
    Escape,
    EscapeSeq,
}

fn print_shell_intro() {
    println!("Welcome to AxVisor Shell!");
    println!("Type 'help' to see available commands");
    println!("Use UP/DOWN arrows to navigate command history");
    print_console_shortcuts();
    #[cfg(not(feature = "fs"))]
    println!("Note: Running with limited features (filesystem support disabled).");
    println!();
}

fn print_console_shortcuts() {
    println!("Console shortcuts:");
    println!("  Ctrl+X, then h  return to the Axvisor shell");
    println!("  Ctrl+X, then [  attach the previous running guest");
    println!("  Ctrl+X, then ]  attach the next running guest");
}

/// Executes one complete command for a connection-local network shell.
///
/// Returns `false` for `exit` and `quit`, which disconnect only that network
/// client instead of shutting down the hypervisor.
#[cfg(feature = "browser-console")]
pub(crate) fn run_network_command(input: &str) -> bool {
    let command = input.trim();
    if matches!(command, "exit" | "quit") {
        return false;
    }

    NETWORK_OUTPUT_SELECTED.with(|selected| {
        let previous = selected.replace(true);
        if !command.is_empty() && !handle_builtin_commands(command) {
            run_cmd_bytes(command.as_bytes());
        }
        selected.set(previous);
    });
    true
}

#[cfg(feature = "browser-console")]
pub(crate) fn network_prompt() -> String {
    prompt_string()
}

fn route_pending_host_log(
    record: &[u8],
    edit_line: &[u8],
    cursor: usize,
    line_len: usize,
    dropped_records: usize,
    dropped_bytes: usize,
) -> bool {
    let Some(output) = crate::guest_console::route_host_log(record, dropped_records, dropped_bytes)
    else {
        return true;
    };

    if crate::guest_console::attached_vm().is_some() {
        crate::guest_console::submit_host_bytes(&output);
        return true;
    }

    let content = std::str::from_utf8(&edit_line[..line_len]).unwrap_or("");
    let prompt = prompt_string();
    let mut transaction = std::vec::Vec::with_capacity(
        output
            .len()
            .saturating_add(prompt.len())
            .saturating_add(content.len())
            .saturating_add(32),
    );
    transaction.extend_from_slice(b"\r\x1b[2K");
    transaction.extend_from_slice(&output);
    transaction.extend_from_slice(prompt.as_bytes());
    transaction.extend_from_slice(content.as_bytes());
    if cursor < content.len() {
        write!(transaction, "\x1b[{}D", content.len() - cursor).ok();
    }
    crate::guest_console::submit_host_bytes(&transaction);
    true
}

// Initialize the console shell.
pub fn console_init() {
    let mut history = CommandHistory::new(100);

    let mut buf = [0; MAX_LINE_LEN];
    let mut cursor = 0; // cursor position in buffer
    let mut line_len = 0; // actual length of current line

    let mut input_state = InputState::Normal;
    let mut pending_shell_byte = None;
    let mut shell_announced = false;

    if crate::guest_console::attached_vm().is_none() {
        print_shell_intro();
        shell_announced = true;
        print_prompt();
    }

    loop {
        if let Some(vm_id) = crate::guest_console::reconcile_vm_states() {
            println!();
            println!("[Axvisor] VM[{vm_id}] stopped; returning to the management shell");
            if !shell_announced {
                print_shell_intro();
                shell_announced = true;
            }
            let current_content = std::str::from_utf8(&buf[..line_len]).unwrap_or("");
            redraw_shell_line(&prompt_string(), current_content, cursor);
        }

        let dropped = crate::guest_console::take_host_log_drops();
        if let Some(record) = crate::guest_console::read_host_log() {
            route_pending_host_log(
                record.bytes(),
                &buf,
                cursor,
                line_len,
                dropped.records,
                dropped.source_bytes,
            );
            continue;
        }
        if dropped.records != 0 {
            route_pending_host_log(
                &[],
                &buf,
                cursor,
                line_len,
                dropped.records,
                dropped.source_bytes,
            );
            continue;
        }

        let ch = match pending_shell_byte.take() {
            Some(ch) => ch,
            None => {
                let Some(host_byte) = crate::guest_console::read_host_byte() else {
                    crate::guest_console::wait_for_host_event();
                    continue;
                };

                match crate::guest_console::route_host_byte(host_byte) {
                    ConsoleInputEvent::ShellByte(ch) => ch,
                    ConsoleInputEvent::ShellSequence(first, second) => {
                        pending_shell_byte = Some(second);
                        first
                    }
                    ConsoleInputEvent::Consumed => continue,
                    ConsoleInputEvent::Attached(vm_id) => {
                        println!();
                        println!(
                            "[Axvisor] attached VM[{vm_id}] console; use Ctrl+X, then h to return \
                             to the shell"
                        );
                        crate::guest_console::activate(vm_id);
                        continue;
                    }
                    ConsoleInputEvent::Detached(vm_id) => {
                        println!();
                        println!("[Axvisor] detached VM[{vm_id}] console");
                        if !shell_announced {
                            print_shell_intro();
                            shell_announced = true;
                        }
                        let current_content = std::str::from_utf8(&buf[..line_len]).unwrap_or("");
                        redraw_shell_line(&prompt_string(), current_content, cursor);
                        continue;
                    }
                    ConsoleInputEvent::NoRunningGuest => {
                        println!();
                        println!("[Axvisor] no running VM is available for console attachment");
                        let current_content = std::str::from_utf8(&buf[..line_len]).unwrap_or("");
                        redraw_shell_line(&prompt_string(), current_content, cursor);
                        continue;
                    }
                }
            }
        };

        match input_state {
            InputState::Normal => {
                match ch {
                    CR | LF => {
                        println!();
                        if line_len > 0 {
                            let cmd_str = std::str::from_utf8(&buf[..line_len]).unwrap_or("");

                            // Add to history
                            history.add_command(cmd_str.to_string());

                            // Execute command
                            if !handle_builtin_commands(cmd_str) {
                                run_cmd_bytes(&buf[..line_len]);
                            }

                            // reset buffer
                            buf[..line_len].fill(0);
                            cursor = 0;
                            line_len = 0;
                        }
                        if crate::guest_console::attached_vm().is_none() {
                            print_prompt();
                        }
                    }
                    BS | DL => {
                        // backspace: delete character before cursor / DEL key: delete character at cursor
                        if cursor > 0 {
                            // move characters after cursor forward
                            for i in cursor..line_len {
                                buf[i - 1] = buf[i];
                            }
                            cursor -= 1;
                            line_len -= 1;
                            if line_len < buf.len() {
                                buf[line_len] = 0;
                            }

                            let current_content =
                                std::str::from_utf8(&buf[..line_len]).unwrap_or("");
                            let prompt = prompt_string();
                            redraw_shell_line(&prompt, current_content, cursor);
                        }
                    }
                    ESC => {
                        input_state = InputState::Escape;
                    }
                    0..=31 => {
                        // ignore other control characters
                    }
                    c => {
                        // insert character
                        if line_len < MAX_LINE_LEN - 1 {
                            // move characters after cursor backward to make space for new character
                            for i in (cursor..line_len).rev() {
                                buf[i + 1] = buf[i];
                            }
                            buf[cursor] = c;
                            cursor += 1;
                            line_len += 1;

                            let current_content =
                                std::str::from_utf8(&buf[..line_len]).unwrap_or("");
                            let prompt = prompt_string();
                            redraw_shell_line(&prompt, current_content, cursor);
                        }
                    }
                }
            }
            InputState::Escape => match ch {
                b'[' => {
                    input_state = InputState::EscapeSeq;
                }
                _ => {
                    input_state = InputState::Normal;
                }
            },
            InputState::EscapeSeq => {
                match ch {
                    b'A' => {
                        // UP arrow - previous command
                        if let Some(prev_cmd) = history.previous() {
                            // clear current buffer
                            buf[..line_len].fill(0);

                            let cmd_bytes = prev_cmd.as_bytes();
                            let copy_len = cmd_bytes.len().min(MAX_LINE_LEN - 1);
                            buf[..copy_len].copy_from_slice(&cmd_bytes[..copy_len]);
                            cursor = copy_len;
                            line_len = copy_len;
                            let prompt = prompt_string();
                            redraw_shell_line(&prompt, prev_cmd, cursor);
                        }
                        input_state = InputState::Normal;
                    }
                    b'B' => {
                        // DOWN arrow - next command
                        match history.next() {
                            Some(next_cmd) => {
                                // clear current buffer
                                buf[..line_len].fill(0);

                                let cmd_bytes = next_cmd.as_bytes();
                                let copy_len = cmd_bytes.len().min(MAX_LINE_LEN - 1);
                                buf[..copy_len].copy_from_slice(&cmd_bytes[..copy_len]);
                                cursor = copy_len;
                                line_len = copy_len;

                                let prompt = prompt_string();
                                redraw_shell_line(&prompt, next_cmd, cursor);
                            }
                            None => {
                                // clear current line
                                buf[..line_len].fill(0);
                                cursor = 0;
                                line_len = 0;
                                let prompt = prompt_string();
                                redraw_shell_line(&prompt, "", cursor);
                            }
                        }
                        input_state = InputState::Normal;
                    }
                    b'C' => {
                        // RIGHT arrow - move cursor right
                        if cursor < line_len {
                            cursor += 1;
                            crate::guest_console::submit_host_bytes(b"\x1b[C");
                        }
                        input_state = InputState::Normal;
                    }
                    b'D' => {
                        // LEFT arrow - move cursor left
                        if cursor > 0 {
                            cursor -= 1;
                            crate::guest_console::submit_host_bytes(b"\x1b[D");
                        }
                        input_state = InputState::Normal;
                    }
                    b'3' => {
                        // check if this is Delete key sequence (ESC[3~)
                        // need to read next character to confirm
                        input_state = InputState::Normal;
                        // can add additional state to handle complete Delete sequence
                    }
                    _ => {
                        // ignore other escape sequences
                        input_state = InputState::Normal;
                    }
                }
            }
        }
    }
}

fn redraw_shell_line(prompt: &str, content: &str, cursor: usize) {
    let output = redraw_line(prompt, content, cursor);
    crate::guest_console::submit_host_bytes(&output);
}
