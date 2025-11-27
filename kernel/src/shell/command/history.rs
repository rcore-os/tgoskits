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
