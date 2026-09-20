//! Completion for unquoted command words and filesystem paths.

use std::string::String;
#[cfg(feature = "fs")]
use std::string::ToString;

use super::{command, prompt_string, redraw_shell_line};

pub(super) fn complete_line(buf: &mut [u8], line_len: &mut usize, cursor: &mut usize) {
    // Completing inside a word would duplicate its untouched suffix.
    if *cursor < *line_len && !buf[*cursor].is_ascii_whitespace() {
        return;
    }
    let Ok(before_cursor) = std::str::from_utf8(&buf[..*cursor]) else {
        return;
    };
    // Leave quoted/escaped words to the command parser rather than interpreting
    // a different token boundary during completion.
    if before_cursor.contains(['\'', '"', '\\']) {
        return;
    }
    let start = before_cursor
        .rfind(|ch: char| ch.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    let prefix = &before_cursor[start..];
    let context = &before_cursor[..start];
    let mut candidates = command::command_completions(context, prefix);
    #[cfg(feature = "fs")]
    if !context.trim().is_empty() || prefix.contains('/') {
        candidates.extend(path_completions(prefix));
    }
    candidates.sort();
    candidates.dedup();
    let Some(first) = candidates.first() else {
        return;
    };
    let mut common_len = first.len();
    for candidate in &candidates[1..] {
        common_len = first
            .bytes()
            .zip(candidate.bytes())
            .take(common_len)
            .take_while(|(left, right)| left == right)
            .count();
    }
    while !first.is_char_boundary(common_len) {
        common_len -= 1;
    }
    let mut suffix = String::from(&first[prefix.len()..common_len]);
    if candidates.len() == 1 && !first.ends_with('/') && *cursor == *line_len {
        suffix.push(' ');
    }
    if *line_len + suffix.len() >= buf.len() {
        return;
    }
    buf.copy_within(*cursor..*line_len, *cursor + suffix.len());
    buf[*cursor..*cursor + suffix.len()].copy_from_slice(suffix.as_bytes());
    *cursor += suffix.len();
    *line_len += suffix.len();
    if candidates.len() > 1 && suffix.is_empty() {
        println!();
        for candidate in candidates {
            print!("{}  ", candidate);
        }
        println!();
    }
    let content = std::str::from_utf8(&buf[..*line_len]).unwrap_or("");
    redraw_shell_line(&prompt_string(), content, *cursor);
}

#[cfg(feature = "fs")]
fn path_completions(prefix: &str) -> std::vec::Vec<String> {
    let (directory, basename) = prefix
        .rsplit_once('/')
        .map_or(("", prefix), |(dir, name)| (&prefix[..dir.len() + 1], name));
    let Ok(entries) = std::fs::read_dir(if directory.is_empty() { "." } else { directory }) else {
        return std::vec::Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let name = file_name.to_str()?;
            // Some builtins interpret redirection after shlex parsing. Quoting is
            // therefore insufficient: offer only names with no shell syntax or
            // terminal control bytes until those commands share a lexer.
            if !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
            {
                return None;
            }
            if !name.starts_with(basename) || (name.starts_with('.') && !basename.starts_with('.'))
            {
                return None;
            }
            let mut candidate = directory.to_string();
            candidate.push_str(name);
            if entry.file_type().ok()?.is_dir() {
                candidate.push('/');
            }
            Some(candidate)
        })
        .collect()
}
