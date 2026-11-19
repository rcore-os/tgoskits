//! Avoid rescanning old lines when a failure pattern cannot consume a newline.

use anyhow::Result;
use regex::bytes::RegexSet;
use regex_automata::nfa::thompson::{NFA, State};

pub(super) struct FailurePatterns {
    patterns: Vec<String>,
    local: RegexSet,
    local_ids: Vec<usize>,
    spanning: RegexSet,
    spanning_ids: Vec<usize>,
}

impl FailurePatterns {
    pub(super) fn new(patterns: &[String]) -> Result<Self> {
        // Validate the original string-regex contract before using byte offsets.
        regex::RegexSet::new(patterns)?;
        let mut local_ids = Vec::new();
        let mut spanning_ids = Vec::new();
        for (index, pattern) in patterns.iter().enumerate() {
            if line_local(pattern)? {
                local_ids.push(index);
            } else {
                spanning_ids.push(index);
            }
        }
        Ok(Self {
            local: RegexSet::new(local_ids.iter().map(|&i| &patterns[i]))?,
            spanning: RegexSet::new(spanning_ids.iter().map(|&i| &patterns[i]))?,
            patterns: patterns.to_vec(),
            local_ids,
            spanning_ids,
        })
    }

    pub(super) fn matching(&self, transcript: &[u8], line_start: usize) -> Option<&str> {
        let index = self
            .local
            .matches_at(transcript, line_start)
            .iter()
            .next()
            .map(|i| self.local_ids[i])
            .or_else(|| {
                self.spanning
                    .matches(transcript)
                    .iter()
                    .next()
                    .map(|i| self.spanning_ids[i])
            });
        index.map(|i| self.patterns[i].as_str())
    }
}

fn line_local(pattern: &str) -> Result<bool> {
    let nfa = NFA::new(pattern)?;
    let mut visited = vec![false; nfa.states().len()];
    // Starting at the anchored entry excludes the unanchored search prefix.
    let mut pending = vec![nfa.start_anchored()];
    while let Some(id) = pending.pop() {
        if visited[id.as_usize()] {
            continue;
        }
        visited[id.as_usize()] = true;
        match nfa.state(id) {
            State::ByteRange { trans } => {
                if trans.matches_byte(b'\n') {
                    return Ok(false);
                }
                pending.push(trans.next);
            }
            State::Sparse(transitions) => {
                if transitions.matches_byte(b'\n').is_some() {
                    return Ok(false);
                }
                pending.extend(transitions.transitions.iter().map(|t| t.next));
            }
            State::Dense(transitions) => {
                if transitions.matches_byte(b'\n').is_some() {
                    return Ok(false);
                }
                pending.extend(transitions.transitions.iter().copied());
            }
            State::Look { next, .. } | State::Capture { next, .. } => pending.push(*next),
            State::Union { alternates } => pending.extend(alternates.iter().copied()),
            State::BinaryUnion { alt1, alt2 } => pending.extend([*alt1, *alt2]),
            State::Fail | State::Match { .. } => {}
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_search_retains_unicode_anchors_and_cross_line_failures() {
        let patterns = [
            r"(?i)\bpanic\b",
            r"\Afirst",
            r"begin\s+end",
            r"(?m)^VM\[\d+\] failed$",
        ]
        .map(String::from);
        let matcher = FailurePatterns::new(&patterns).unwrap();
        assert!(line_local(&patterns[0]).unwrap());
        assert!(!line_local(&patterns[2]).unwrap());
        let reference = regex::RegexSet::new(&patterns).unwrap();
        for text in [
            "épanic\nsecond",
            "safe\nfirst",
            "safe\nbegin\nend",
            "safe\nVM[321] failed",
            "safe\nPANIC",
        ] {
            let mut bytes = Vec::new();
            let mut line_start = 0;
            for byte in text.bytes() {
                bytes.push(byte);
                if let Ok(valid) = std::str::from_utf8(&bytes) {
                    assert_eq!(
                        matcher.matching(&bytes, line_start).is_some(),
                        reference.is_match(valid),
                        "{valid:?}"
                    );
                }
                if byte == b'\n' {
                    line_start = bytes.len();
                }
            }
        }
    }
}
