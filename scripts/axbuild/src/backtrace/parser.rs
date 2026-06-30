use std::collections::HashSet;

use anyhow::Context;
use regex::Regex;

#[derive(Debug, Clone)]
pub(super) struct Frame {
    pub(super) idx: usize,
    pub(super) ip: u64,
    pub(super) fp: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct Block {
    pub(super) kind: String,
    pub(super) arch: Option<String>,
    pub(super) frames: Vec<Frame>,
    pub(super) errors: Vec<String>,
}

fn case_name_kind_hint(case_name: &str) -> Option<&'static str> {
    const KINDS: &[&str] = &["raw", "panic", "trap"];
    for segment in case_name.split(['/', '-']) {
        for kind in KINDS {
            if segment == *kind {
                return Some(kind);
            }
        }
    }
    if case_name.ends_with("-raw") {
        return Some("raw");
    }
    if case_name.ends_with("-panic") {
        return Some("panic");
    }
    if case_name.ends_with("-trap") {
        return Some("trap");
    }
    None
}

/// Infer a `kind=` filter for symbolize: case-name hints, else single block kind, else all kinds.
pub(super) fn infer_kind_filter(case_name: &str, blocks: &[Block]) -> Option<String> {
    if let Some(kind) = case_name_kind_hint(case_name) {
        return Some(kind.to_string());
    }

    let kinds: HashSet<&str> = blocks.iter().map(|block| block.kind.as_str()).collect();
    if kinds.len() == 1 {
        return kinds.into_iter().next().map(str::to_string);
    }
    None
}

pub(super) fn parse_blocks(text: &str) -> anyhow::Result<Vec<Block>> {
    let begin_re = Regex::new(r"BACKTRACE_BEGIN\b.*\bkind=([^\s]+)\b(?:.*\barch=([^\s]+)\b)?")
        .context("invalid begin regex")?;
    let frame_re = Regex::new(r"\bBT\s+(\d+)\s+ip=0x([0-9a-fA-F]+)(?:\s+fp=0x([0-9a-fA-F]+))?")
        .context("invalid frame regex")?;
    let error_re = Regex::new(r"\bBT_ERROR\s+([^\s]+)").context("invalid error regex")?;
    let end_re = Regex::new(r"BACKTRACE_END\b").context("invalid end regex")?;

    #[derive(Debug)]
    enum State {
        Idle,
        Capturing(Block),
    }

    let mut state = State::Idle;
    let mut out = Vec::new();

    for line in text.lines() {
        match &mut state {
            State::Idle => {
                if let Some(cap) = begin_re.captures(line) {
                    let kind = cap.get(1).unwrap().as_str().to_string();
                    let arch = cap.get(2).map(|m| m.as_str().to_string());
                    state = State::Capturing(Block {
                        kind,
                        arch,
                        frames: Vec::new(),
                        errors: Vec::new(),
                    });
                }
            }
            State::Capturing(block) => {
                if let Some(cap) = begin_re.captures(line) {
                    out.push(block.clone());
                    let kind = cap.get(1).unwrap().as_str().to_string();
                    let arch = cap.get(2).map(|m| m.as_str().to_string());
                    *block = Block {
                        kind,
                        arch,
                        frames: Vec::new(),
                        errors: Vec::new(),
                    };
                    continue;
                }
                if end_re.is_match(line) {
                    out.push(block.clone());
                    state = State::Idle;
                    continue;
                }

                if let Some(cap) = frame_re.captures(line) {
                    let idx: usize = cap.get(1).unwrap().as_str().parse()?;
                    let ip = u64::from_str_radix(cap.get(2).unwrap().as_str(), 16)?;
                    let fp = cap
                        .get(3)
                        .map(|m| u64::from_str_radix(m.as_str(), 16))
                        .transpose()?;
                    block.frames.push(Frame { idx, ip, fp });
                    continue;
                }

                if let Some(cap) = error_re.captures(line) {
                    let err = cap.get(1).unwrap().as_str().to_string();
                    block.errors.push(err);
                }
            }
        }
    }

    if let State::Capturing(block) = state {
        out.push(block);
    }

    Ok(out)
}
