//! Linux-style kernel command-line tokenization for host root selection.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

pub fn tokens(command_line: &str) -> Vec<String> {
    let bytes = command_line.as_bytes();
    let mut result = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        while position < bytes.len() && bytes[position].is_ascii_whitespace() {
            position += 1;
        }
        if position == bytes.len() {
            break;
        }
        let initially_quoted = bytes[position] == b'"';
        if initially_quoted {
            position += 1;
        }
        let start = position;
        let mut quoted = initially_quoted;
        let mut equals = None;
        while position < bytes.len() {
            let byte = bytes[position];
            if byte.is_ascii_whitespace() && !quoted {
                break;
            }
            if byte == b'=' && equals.is_none() {
                equals = Some(position - start);
            }
            if byte == b'"' {
                quoted = !quoted;
            }
            position += 1;
        }
        let mut token = command_line[start..position].to_string();
        if initially_quoted && token.ends_with('"') {
            token.pop();
        }
        if let Some(equals) = equals
            && token.as_bytes().get(equals + 1) == Some(&b'"')
        {
            token.remove(equals + 1);
            if token.ends_with('"') {
                token.pop();
            }
        }
        result.push(token);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::tokens;

    #[test]
    fn quoted_values_and_delimiter_follow_linux_next_arg() {
        assert_eq!(
            tokens("root=/dev/sda env=\"two words\" \"one arg\" -- root=/ignored"),
            [
                "root=/dev/sda",
                "env=two words",
                "one arg",
                "--",
                "root=/ignored"
            ]
        );
    }
}
