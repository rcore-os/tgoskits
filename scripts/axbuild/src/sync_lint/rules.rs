use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use proc_macro2::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rule {
    WaitCondition,
    PublishBeforeNotify,
    MixedOrdering,
}

impl Rule {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::WaitCondition => "suspicious_relaxed_wait_condition",
            Self::PublishBeforeNotify => "suspicious_relaxed_publish_before_notify",
            Self::MixedOrdering => "suspicious_relaxed_mixed_ordering",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AccessOrdering {
    Relaxed,
    Strong,
}

#[derive(Debug, Clone)]
pub(super) struct AtomicAccess {
    pub(super) key: String,
    pub(super) span: Span,
    pub(super) ordering: AccessOrdering,
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct AccessSummary {
    pub(super) has_relaxed: bool,
    pub(super) has_strong: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Finding {
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) rule: Rule,
    pub(super) message: String,
}

impl Finding {
    pub(super) fn new(path: &Path, span: Span, rule: Rule, message: &'static str) -> Self {
        let start = span.start();
        Self {
            path: path.to_path_buf(),
            line: start.line,
            column: start.column + 1,
            rule,
            message: message.to_string(),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct AnalysisResult {
    pub(super) accesses: Vec<AtomicAccess>,
    pub(super) sync_intent_keys: HashSet<String>,
    pub(super) findings: Vec<Finding>,
}
