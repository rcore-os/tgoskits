use std::time::{Duration, Instant};

use anyhow::anyhow;
use colored::Colorize;
use regex::Regex;

pub(crate) const MATCH_DRAIN_DURATION: Duration = Duration::from_millis(500);
const MAX_MATCH_WINDOW_BYTES: usize = 2048;
const DEFAULT_FAIL_PATTERNS: &[&str] = &[r"(?i)\bpanic(?:ked)?\b", r"(?i)kernel panic"];
const MATCH_EXCERPT_CONTEXT_CHARS: usize = 120;
const MATCH_EXCERPT_MAX_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamMatchKind {
    Success,
    Fail,
}

impl StreamMatchKind {
    pub(crate) fn into_result(self, matched: &StreamMatch) -> anyhow::Result<()> {
        match self {
            StreamMatchKind::Success => Ok(()),
            StreamMatchKind::Fail => Err(anyhow!(
                "Fail pattern matched '{}': {}",
                matched.matched_regex,
                match_excerpt(&matched.matched_text, &matched.matched_regex)
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StreamMatch {
    pub kind: StreamMatchKind,
    pub matched_regex: String,
    pub matched_text: String,
    pub deadline: Instant,
}

pub(crate) fn compile_regexes(
    success_patterns: &[String],
    fail_patterns: &[String],
) -> anyhow::Result<(Vec<Regex>, Vec<Regex>)> {
    let success_regex = success_patterns
        .iter()
        .map(|p| Regex::new(p).map_err(|e| anyhow!("success regex error: {e}")))
        .collect::<Result<Vec<_>, _>>()?;

    let mut merged_fail_patterns = fail_patterns.to_vec();
    for pattern in DEFAULT_FAIL_PATTERNS {
        if !merged_fail_patterns
            .iter()
            .any(|existing| existing == pattern)
        {
            merged_fail_patterns.push((*pattern).to_string());
        }
    }

    let fail_regex = merged_fail_patterns
        .iter()
        .map(|p| Regex::new(p).map_err(|e| anyhow!("fail regex error: {e}")))
        .collect::<Result<Vec<_>, _>>()?;

    Ok((success_regex, fail_regex))
}

pub(crate) fn print_match_event(matched: &StreamMatch) {
    match matched.kind {
        StreamMatchKind::Success => println!(
            "{}",
            format!(
                "\n=== SUCCESS PATTERN MATCHED: {} ===",
                matched.matched_regex
            )
            .green()
        ),
        StreamMatchKind::Fail => println!(
            "{}",
            format!("\n=== FAIL PATTERN MATCHED: {}", matched.matched_regex).red()
        ),
    }
}

#[derive(Debug, Clone)]
enum StreamMatchState {
    Pending,
    Matched(StreamMatch),
}

pub struct ByteStreamMatcher {
    success_regex: Vec<Regex>,
    fail_regex: Vec<Regex>,
    match_buf: Vec<u8>,
    state: StreamMatchState,
}

impl ByteStreamMatcher {
    pub fn new(success_regex: Vec<Regex>, fail_regex: Vec<Regex>) -> Self {
        Self {
            success_regex,
            fail_regex,
            match_buf: Vec::with_capacity(MAX_MATCH_WINDOW_BYTES),
            state: StreamMatchState::Pending,
        }
    }

    pub fn observe_byte(&mut self, byte: u8) -> Option<StreamMatch> {
        self.match_buf.push(byte);
        if self.match_buf.len() > MAX_MATCH_WINDOW_BYTES {
            let overflow = self.match_buf.len() - MAX_MATCH_WINDOW_BYTES;
            self.match_buf.drain(..overflow);
        }

        match self.state {
            StreamMatchState::Pending => {
                let text = String::from_utf8_lossy(&self.match_buf);
                let text = strip_ansi_escape_sequences(&text);

                let matched = self
                    .fail_regex
                    .iter()
                    .find(|regex| regex.is_match(&text))
                    .map(|regex| StreamMatch {
                        kind: StreamMatchKind::Fail,
                        matched_regex: regex.as_str().to_string(),
                        matched_text: text.to_string(),
                        deadline: Instant::now() + MATCH_DRAIN_DURATION,
                    })
                    .or_else(|| {
                        self.success_regex
                            .iter()
                            .find(|regex| regex.is_match(&text))
                            .map(|regex| StreamMatch {
                                kind: StreamMatchKind::Success,
                                matched_regex: regex.as_str().to_string(),
                                matched_text: text.to_string(),
                                deadline: Instant::now() + MATCH_DRAIN_DURATION,
                            })
                    });

                if let Some(matched) = matched {
                    self.state = StreamMatchState::Matched(matched.clone());
                    Some(matched)
                } else {
                    None
                }
            }
            StreamMatchState::Matched(_) => None,
        }
    }

    pub fn matched(&self) -> Option<&StreamMatch> {
        match &self.state {
            StreamMatchState::Pending => None,
            StreamMatchState::Matched(matched) => Some(matched),
        }
    }

    pub fn should_stop(&self) -> bool {
        self.matched()
            .is_some_and(|matched| Instant::now() >= matched.deadline)
    }
}

fn strip_ansi_escape_sequences(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == 0x1b
            && let Some(next) = bytes.get(index + 1)
            && *next == b'['
        {
            index += 2;
            while index < bytes.len() {
                let byte = bytes[index];
                index += 1;
                if (0x40..=0x7e).contains(&byte) {
                    break;
                }
            }
            continue;
        }

        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
}

fn match_excerpt(text: &str, matched_regex: &str) -> String {
    let compact = text.replace(['\r', '\n'], " ");
    let compact = compact.split_whitespace().collect::<Vec<_>>().join(" ");

    let Some(regex) = Regex::new(matched_regex).ok() else {
        return truncate_chars(&compact, MATCH_EXCERPT_MAX_CHARS);
    };

    let Some(found) = regex.find(&compact) else {
        return truncate_chars(&compact, MATCH_EXCERPT_MAX_CHARS);
    };

    let start = char_boundary_before(&compact, found.start(), MATCH_EXCERPT_CONTEXT_CHARS);
    let end = char_boundary_after(&compact, found.end(), MATCH_EXCERPT_CONTEXT_CHARS);
    let excerpt = compact[start..end].trim();
    let mut rendered = excerpt.to_string();

    if start > 0 {
        rendered.insert_str(0, "...");
    }
    if end < compact.len() {
        rendered.push_str("...");
    }

    truncate_chars(&rendered, MATCH_EXCERPT_MAX_CHARS)
}

fn char_boundary_before(text: &str, byte_index: usize, chars: usize) -> usize {
    text[..byte_index]
        .char_indices()
        .rev()
        .nth(chars)
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn char_boundary_after(text: &str, byte_index: usize, chars: usize) -> usize {
    text[byte_index..]
        .char_indices()
        .nth(chars)
        .map(|(offset, _)| byte_index + offset)
        .unwrap_or(text.len())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut iter = text.chars();
    let truncated = iter.by_ref().take(max_chars).collect::<String>();
    if iter.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ByteStreamMatcher, StreamMatchKind, compile_regexes, match_excerpt,
        strip_ansi_escape_sequences,
    };
    use regex::Regex;

    #[test]
    fn strips_basic_csi_sequences() {
        assert_eq!(
            strip_ansi_escape_sequences("\u{1b}[31mpanicked at test\u{1b}[m"),
            "panicked at test"
        );
    }

    #[test]
    fn fail_matcher_ignores_ansi_sequences() {
        let mut matcher =
            ByteStreamMatcher::new(vec![], vec![Regex::new("(?i)\\bpanic(?:ked)?\\b").unwrap()]);

        let input = "\u{1b}[31mpanicked at os/arceos/foo.rs:1:1\n";
        let mut matched = None;
        for byte in input.bytes() {
            matched = matcher.observe_byte(byte).or(matched);
        }

        let matched = matched.expect("expected panic match");
        assert_eq!(matched.kind, StreamMatchKind::Fail);
        assert!(matched.matched_text.to_ascii_lowercase().contains("panic"));
    }

    #[test]
    fn matcher_detects_fail_pattern_across_multiple_lines() {
        let mut matcher = ByteStreamMatcher::new(
            vec![],
            vec![Regex::new("Failed to load VM images").unwrap()],
        );

        let input = "line one\nline two\npanicked at foo\nFailed to load VM images: AxErrorKind::NotFound\n";
        let mut matched = None;
        for byte in input.bytes() {
            matched = matcher.observe_byte(byte).or(matched);
        }

        let matched = matched.expect("expected match");
        assert_eq!(matched.kind, StreamMatchKind::Fail);
        assert!(matched.matched_text.contains("Failed to load VM images"));
    }

    #[test]
    fn compile_regexes_appends_builtin_panic_patterns() {
        let (_success, fail) = compile_regexes(&[], &[]).unwrap();

        assert!(
            fail.iter()
                .any(|regex| regex.as_str() == r"(?i)\bpanic(?:ked)?\b")
        );
        assert!(
            fail.iter()
                .any(|regex| regex.as_str() == r"(?i)kernel panic")
        );
    }

    #[test]
    fn match_excerpt_returns_local_context_only() {
        let text = "prefix text one two three panic happened in vm loader because image missing suffix text four five";
        let excerpt = match_excerpt(text, r"(?i)\bpanic\b");

        assert!(excerpt.contains("panic"));
        assert!(!excerpt.is_empty());
        assert!(excerpt.len() <= text.len() + 3);
    }
}
