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

use std::io::Write;
use std::{string::String, vec::Vec};

use super::HISTORY_USAGE;

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

    /// Records a command unless it is blank or repeats the newest entry.
    pub fn add_command(&mut self, cmd: String) {
        if !cmd.trim().is_empty() && self.history.last() != Some(&cmd) {
            if self.history.len() >= self.max_size {
                self.history.remove(0);
            }
            self.history.push(cmd);
        }
        self.current_index = self.history.len();
    }

    /// Removes every retained command and resets arrow-key navigation.
    pub fn clear(&mut self) {
        self.history.clear();
        self.current_index = 0;
    }

    /// Iterates over retained commands from oldest to newest.
    pub fn entries(&self) -> impl Iterator<Item = &str> {
        self.history.iter().map(String::as_str)
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

/// Executes `history` using the console-owned command history.
///
/// A single optional `grep PATTERN` stage filters raw command text by a
/// case-sensitive literal substring. Rejected input never clears entries.
pub(super) fn handle_history_command(tokens: &[String], history: &mut CommandHistory) {
    let (command_tokens, filter) = match split_history_filter(tokens) {
        Ok(parts) => parts,
        Err(message) => {
            print_history_usage_error(&message);
            return;
        }
    };

    match command_tokens {
        [command] if command == "history" => {
            print_history(history, None, filter);
        }
        [command, option] if command == "history" && option == "-c" => {
            if filter.is_some() {
                print_history_usage_error("cannot combine -c with a grep filter");
                return;
            }
            history.clear();
        }
        [command, option] if command == "history" => {
            if !option.starts_with('-') {
                print_history_usage_error("unsupported option or argument");
                return;
            }
            let Some(count) = parse_history_count(option) else {
                print_history_usage_error(&format!(
                    "invalid count '{option}'; expected a positive decimal integer"
                ));
                return;
            };
            print_history(history, Some(count), filter);
        }
        _ => print_history_usage_error("unsupported option or argument"),
    }
}

fn split_history_filter(tokens: &[String]) -> Result<(&[String], Option<&str>), String> {
    let Some(pipe_index) = tokens
        .iter()
        .position(|token| token == "|" || token == "|grep")
    else {
        if tokens.iter().any(|token| token.contains('|')) {
            return Err(
                "unsupported pipeline syntax; only '| grep PATTERN' is supported".to_string(),
            );
        }
        return Ok((tokens, None));
    };

    let pipe = &tokens[pipe_index];
    let right = &tokens[pipe_index + 1..];
    let pattern_tokens = if pipe == "|grep" {
        right
    } else {
        match right {
            [grep, rest @ ..] if grep == "grep" => rest,
            [grep, ..] => {
                return Err(format!(
                    "unsupported filter command '{grep}'; only grep is supported"
                ));
            }
            [] => return Err("grep requires exactly one pattern".to_string()),
        }
    };

    if let Some(option) = pattern_tokens.iter().find(|token| token.starts_with('-')) {
        return Err(format!("grep options are not supported: '{option}'"));
    }
    let [pattern] = pattern_tokens else {
        return Err(
            "grep requires exactly one pattern; additional arguments or pipelines are not \
             supported"
                .to_string(),
        );
    };
    if pattern.is_empty() {
        return Err("grep pattern must not be empty".to_string());
    }

    Ok((&tokens[..pipe_index], Some(pattern.as_str())))
}

fn parse_history_count(option: &str) -> Option<usize> {
    let digits = option.strip_prefix('-')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let count = digits.parse::<usize>().ok()?;
    (count > 0).then_some(count)
}

fn print_history(history: &CommandHistory, last: Option<usize>, filter: Option<&str>) {
    let skip = match last {
        Some(count) => history.entries().count().saturating_sub(count),
        None => 0,
    };
    for (index, entry) in history.entries().enumerate().skip(skip) {
        if filter.is_some_and(|pattern| !entry.contains(pattern)) {
            continue;
        }
        println!("{:>4}  {}", index + 1, entry);
    }
}

fn print_history_usage_error(message: &str) {
    println!("history: {message}");
    println!("Usage: {HISTORY_USAGE}");
}

pub fn redraw_line(prompt: &str, content: &str, cursor_pos: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(
        prompt
            .len()
            .saturating_add(content.len())
            .saturating_add(16),
    );
    output.extend_from_slice(b"\r\x1b[2K");
    output.extend_from_slice(prompt.as_bytes());
    output.extend_from_slice(content.as_bytes());
    if cursor_pos < content.len() {
        write!(output, "\x1b[{}D", content.len() - cursor_pos)
            .expect("writing into Vec cannot fail");
    }
    output
}
