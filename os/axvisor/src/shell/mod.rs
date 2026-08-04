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

use std::io::prelude::*;
use std::println;
use std::string::ToString;

use crate::guest_console::ConsoleInputEvent;
use crate::shell::command::{
    CommandHistory, clear_line_and_redraw, handle_builtin_commands, print_prompt, prompt_string,
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
    println!("  Ctrl+Alt+H  return to the Axvisor shell");
    println!("  Ctrl+Alt+[  attach the previous running guest");
    println!("  Ctrl+Alt+]  attach the next running guest");
}

// Initialize the console shell.
pub fn console_init() {
    let mut stdout = std::io::stdout();
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
            clear_line_and_redraw(&mut stdout, &prompt_string(), current_content, cursor);
        }

        let ch = match pending_shell_byte.take() {
            Some(ch) => ch,
            None => {
                let Some(host_byte) = crate::guest_console::read_host_byte() else {
                    crate::guest_console::wait_for_host_input();
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
                            "[Axvisor] attached VM[{vm_id}] console; use Ctrl+Alt+H to return to \
                             the shell"
                        );
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
                        clear_line_and_redraw(
                            &mut stdout,
                            &prompt_string(),
                            current_content,
                            cursor,
                        );
                        continue;
                    }
                    ConsoleInputEvent::NoRunningGuest => {
                        println!();
                        println!("[Axvisor] no running VM is available for console attachment");
                        let current_content = std::str::from_utf8(&buf[..line_len]).unwrap_or("");
                        clear_line_and_redraw(
                            &mut stdout,
                            &prompt_string(),
                            current_content,
                            cursor,
                        );
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
                            clear_line_and_redraw(&mut stdout, &prompt, current_content, cursor);
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
                            clear_line_and_redraw(&mut stdout, &prompt, current_content, cursor);
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
                            clear_line_and_redraw(&mut stdout, &prompt, prev_cmd, cursor);
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
                                clear_line_and_redraw(&mut stdout, &prompt, next_cmd, cursor);
                            }
                            None => {
                                // clear current line
                                buf[..line_len].fill(0);
                                cursor = 0;
                                line_len = 0;
                                let prompt = prompt_string();
                                clear_line_and_redraw(&mut stdout, &prompt, "", cursor);
                            }
                        }
                        input_state = InputState::Normal;
                    }
                    b'C' => {
                        // RIGHT arrow - move cursor right
                        if cursor < line_len {
                            cursor += 1;
                            stdout.write_all(b"\x1b[C").ok();
                            stdout.flush().ok();
                        }
                        input_state = InputState::Normal;
                    }
                    b'D' => {
                        // LEFT arrow - move cursor left
                        if cursor > 0 {
                            cursor -= 1;
                            stdout.write_all(b"\x1b[D").ok();
                            stdout.flush().ok();
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
