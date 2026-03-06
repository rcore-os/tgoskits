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
use std::{string::String, vec::Vec};

pub struct CommandHistory {
    history: Vec<String>,
    current_index: usize,
    max_size: usize,
}

impl CommandHistory {
    pub fn new(max_size: usize) -> Self {
        Self {
            history: Vec::new(),
            current_index: 0,
            max_size,
        }
    }

    pub fn add_command(&mut self, cmd: String) {
        if !cmd.trim().is_empty() && self.history.last() != Some(&cmd) {
            if self.history.len() >= self.max_size {
                self.history.remove(0);
            }
            self.history.push(cmd);
        }
        self.current_index = self.history.len();
    }

    #[allow(dead_code)]
    pub fn previous(&mut self) -> Option<&String> {
        if self.current_index > 0 {
            self.current_index -= 1;
            self.history.get(self.current_index)
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub fn next(&mut self) -> Option<&String> {
        if self.current_index < self.history.len() {
            self.current_index += 1;
            if self.current_index < self.history.len() {
                self.history.get(self.current_index)
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[allow(unused_must_use)]
pub fn clear_line_and_redraw(
    stdout: &mut dyn Write,
    prompt: &str,
    content: &str,
    cursor_pos: usize,
) {
    write!(stdout, "\r");
    write!(stdout, "\x1b[2K");
    write!(stdout, "{prompt}{content}");
    if cursor_pos < content.len() {
        write!(stdout, "\x1b[{}D", content.len() - cursor_pos);
    }
    stdout.flush();
}
