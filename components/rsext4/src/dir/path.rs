//! Path normalization helpers for directory operations.

use alloc::string::String;

/// Normalizes a path by collapsing repeated separators and trimming a trailing slash.
pub fn split_paren_child_and_tranlatevalid(pat: &str) -> String {
    let mut last_c = '\0';
    let mut result_s = String::new();
    for ch in pat.chars() {
        if ch == '/' && last_c == '/' {
            continue;
        }
        result_s.push(ch);
        last_c = ch;
    }

    while result_s.len() > 1 && result_s.ends_with('/') {
        result_s.pop();
    }

    result_s
}
