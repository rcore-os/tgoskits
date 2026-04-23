use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use cargo_metadata::{Metadata, Package};
use proc_macro2::Span;
use quote::ToTokens;
use syn::{
    Block, Expr, ExprCall, ExprClosure, ExprMethodCall, ExprPath, ExprWhile, File, Ident,
    ItemMacro, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};
use walkdir::WalkDir;

pub(crate) fn run_sync_lint_command() -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&workspace_manifest)
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let packages = workspace_packages(&metadata);

    println!(
        "running sync-lint for {} workspace package(s) from {}",
        packages.len(),
        workspace_root.display()
    );

    let mut findings = Vec::new();
    for package in &packages {
        findings.extend(package_findings(package)?);
    }

    if findings.is_empty() {
        println!("all sync-lint checks passed");
        return Ok(());
    }

    println!(
        "sync-lint found {} issue(s) across {} file(s):",
        findings.len(),
        findings
            .iter()
            .map(|finding| finding.path.clone())
            .collect::<HashSet<_>>()
            .len()
    );
    for finding in &findings {
        println!(
            "{}:{}:{}: {} [{}]",
            finding.path.display(),
            finding.line,
            finding.column,
            finding.message,
            finding.rule.label()
        );
    }

    bail!("sync-lint found {} issue(s)", findings.len())
}

fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let mut packages: Vec<_> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .cloned()
        .collect();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages
}

fn package_findings(package: &Package) -> anyhow::Result<Vec<Finding>> {
    let package_dir = package
        .manifest_path
        .clone()
        .into_std_path_buf()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("invalid manifest path for package `{}`", package.name))?;

    let mut findings = Vec::new();
    for source_path in rust_source_files(&package_dir) {
        findings.extend(file_findings(&source_path)?);
    }
    Ok(findings)
}

fn rust_source_files(package_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in WalkDir::new(package_dir)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    files
}

fn file_findings(path: &Path) -> anyhow::Result<Vec<Finding>> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let syntax = syn::parse_file(&source)
        .with_context(|| format!("failed to parse Rust file {}", path.display()))?;
    Ok(analyze_file(path, &source, &syntax))
}

fn analyze_file(path: &Path, source: &str, syntax: &File) -> Vec<Finding> {
    let mut analyzer = Analyzer::new(path, source);
    analyzer.visit_file(syntax);
    analyzer.finish();
    analyzer.findings
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    WaitCondition,
    PublishBeforeNotify,
    MixedOrdering,
}

impl Rule {
    fn label(self) -> &'static str {
        match self {
            Self::WaitCondition => "suspicious_relaxed_wait_condition",
            Self::PublishBeforeNotify => "suspicious_relaxed_publish_before_notify",
            Self::MixedOrdering => "suspicious_relaxed_mixed_ordering",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessOrdering {
    Relaxed,
    Strong,
}

#[derive(Debug, Clone)]
struct AtomicAccess {
    key: String,
    span: Span,
    ordering: AccessOrdering,
}

#[derive(Debug, Default, Clone, Copy)]
struct AccessSummary {
    has_relaxed: bool,
    has_strong: bool,
}

#[derive(Debug, Clone)]
struct Finding {
    path: PathBuf,
    line: usize,
    column: usize,
    rule: Rule,
    message: String,
}

struct Analyzer<'a> {
    path: &'a Path,
    lines: Vec<&'a str>,
    accesses: Vec<AtomicAccess>,
    sync_intent_keys: HashSet<String>,
    findings: Vec<Finding>,
}

impl<'a> Analyzer<'a> {
    fn new(path: &'a Path, source: &'a str) -> Self {
        Self {
            path,
            lines: source.lines().collect(),
            accesses: Vec::new(),
            sync_intent_keys: HashSet::new(),
            findings: Vec::new(),
        }
    }

    fn finish(&mut self) {
        let mut summaries: HashMap<String, AccessSummary> = HashMap::new();
        for access in &self.accesses {
            let summary = summaries.entry(access.key.clone()).or_default();
            match access.ordering {
                AccessOrdering::Relaxed => summary.has_relaxed = true,
                AccessOrdering::Strong => summary.has_strong = true,
            }
        }

        let spans = self
            .accesses
            .iter()
            .filter(|access| access.ordering == AccessOrdering::Relaxed)
            .filter(|access| self.sync_intent_keys.contains(&access.key))
            .filter(|access| {
                summaries
                    .get(&access.key)
                    .is_some_and(|summary| summary.has_relaxed && summary.has_strong)
            })
            .map(|access| access.span)
            .collect::<Vec<_>>();

        for span in spans {
            self.report(
                span,
                Rule::MixedOrdering,
                "Relaxed atomic access is mixed with stronger orderings on the same \
                 synchronization variable",
            );
        }
    }

    fn report(&mut self, span: Span, rule: Rule, message: &'static str) {
        let start = span.start();
        if self.is_ignored(rule, start.line) {
            return;
        }

        self.findings.push(Finding {
            path: self.path.to_path_buf(),
            line: start.line,
            column: start.column + 1,
            rule,
            message: message.to_string(),
        });
    }

    fn is_ignored(&self, rule: Rule, line: usize) -> bool {
        let line_indexes = [
            line.saturating_sub(1),
            line.saturating_sub(2),
            line.saturating_sub(3),
        ];
        line_indexes.into_iter().any(|line_no| {
            if line_no == 0 {
                return false;
            }
            self.lines.get(line_no - 1).is_some_and(|line| {
                line.contains("sync-lint: ignore")
                    && (line.contains(rule.label()) || !line.contains("suspicious_relaxed_"))
            })
        })
    }

    fn mark_sync_intent_expr(&mut self, expr: &Expr) {
        for access in atomic_accesses_in_expr(expr) {
            self.sync_intent_keys.insert(access.key);
        }
    }

    fn check_wait_closure(&mut self, closure: &ExprClosure) {
        self.mark_sync_intent_expr(&closure.body);
        if let Some(span) = first_relaxed_load(&closure.body) {
            self.report(
                span,
                Rule::WaitCondition,
                "Relaxed atomic load is used in a wait condition",
            );
        }
    }

    fn check_block_for_publish_before_notify(&mut self, block: &Block) {
        let statements = block
            .stmts
            .iter()
            .filter_map(statement_expr)
            .collect::<Vec<_>>();
        for pair in statements.windows(2) {
            let [first, second] = pair else {
                continue;
            };
            if let Some(access) = atomic_write_access(first)
                && is_notify_expr(second)
            {
                self.sync_intent_keys.insert(access.key);
                if access.ordering == AccessOrdering::Relaxed {
                    self.report(
                        access.span,
                        Rule::PublishBeforeNotify,
                        "Relaxed atomic write is immediately followed by a wake/notify operation",
                    );
                }
            }
        }
    }
}

impl Visit<'_> for Analyzer<'_> {
    fn visit_item_macro(&mut self, node: &ItemMacro) {
        if node
            .mac
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "app")
            && let Ok(file) = syn::parse2::<File>(node.mac.tokens.clone())
        {
            self.visit_file(&file);
        }
        visit::visit_item_macro(self, node);
    }

    fn visit_expr_call(&mut self, node: &ExprCall) {
        if is_wait_function(node) {
            for arg in &node.args {
                if let Expr::Closure(closure) = arg {
                    self.check_wait_closure(closure);
                }
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &ExprMethodCall) {
        if let Some(access) = atomic_access_from_method_call(node) {
            self.accesses.push(access);
        }
        if is_wait_method(node.method.clone()) {
            for arg in &node.args {
                if let Expr::Closure(closure) = arg {
                    self.check_wait_closure(closure);
                }
            }
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_while(&mut self, node: &ExprWhile) {
        if let Some(span) = first_relaxed_load(&node.cond)
            && block_contains_blocking_call(&node.body)
        {
            self.mark_sync_intent_expr(&node.cond);
            self.report(
                span,
                Rule::WaitCondition,
                "Relaxed atomic load is used in a blocking loop condition",
            );
        } else if block_contains_blocking_call(&node.body) {
            self.mark_sync_intent_expr(&node.cond);
        }
        visit::visit_expr_while(self, node);
    }

    fn visit_block(&mut self, node: &Block) {
        self.check_block_for_publish_before_notify(node);
        visit::visit_block(self, node);
    }
}

fn statement_expr(stmt: &Stmt) -> Option<&Expr> {
    match stmt {
        Stmt::Expr(expr, _) => Some(expr),
        _ => None,
    }
}

fn is_wait_function(node: &ExprCall) -> bool {
    function_name(&node.func).is_some_and(|name| {
        matches!(
            name.as_str(),
            "ax_wait_queue_wait_until" | "ax_wait_queue_wait_timeout_until"
        )
    })
}

fn is_wait_method(method: Ident) -> bool {
    matches!(
        method.to_string().as_str(),
        "wait_until" | "wait_timeout_until" | "wait_while"
    )
}

fn function_name(expr: &Expr) -> Option<String> {
    let Expr::Path(ExprPath { path, .. }) = expr else {
        return None;
    };
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn is_notify_expr(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(method) => matches!(
            method.method.to_string().as_str(),
            "notify_one" | "notify_all" | "wake" | "wake_one" | "wake_all" | "unpark"
        ),
        Expr::Call(call) => function_name(&call.func).is_some_and(|name| {
            matches!(
                name.as_str(),
                "ax_wait_queue_wake"
                    | "notify_one"
                    | "notify_all"
                    | "wake"
                    | "wake_one"
                    | "wake_all"
                    | "unpark"
            )
        }),
        _ => false,
    }
}

fn atomic_write_access(expr: &Expr) -> Option<AtomicAccess> {
    let Expr::MethodCall(method) = expr else {
        return None;
    };
    if !matches!(
        method.method.to_string().as_str(),
        "store"
            | "swap"
            | "fetch_add"
            | "fetch_sub"
            | "fetch_or"
            | "fetch_and"
            | "fetch_xor"
            | "fetch_max"
            | "fetch_min"
    ) {
        return None;
    }
    atomic_access_from_method_call(method)
}

fn first_relaxed_load(expr: &Expr) -> Option<Span> {
    let mut finder = RelaxedLoadFinder { span: None };
    finder.visit_expr(expr);
    finder.span
}

struct RelaxedLoadFinder {
    span: Option<Span>,
}

impl Visit<'_> for RelaxedLoadFinder {
    fn visit_expr_method_call(&mut self, node: &ExprMethodCall) {
        if self.span.is_none()
            && node.method == "load"
            && node.args.len() == 1
            && is_relaxed_ordering(&node.args[0])
        {
            self.span = Some(node.span());
            return;
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn atomic_accesses_in_expr(expr: &Expr) -> Vec<AtomicAccess> {
    let mut finder = AtomicAccessFinder {
        accesses: Vec::new(),
    };
    finder.visit_expr(expr);
    finder.accesses
}

struct AtomicAccessFinder {
    accesses: Vec<AtomicAccess>,
}

impl Visit<'_> for AtomicAccessFinder {
    fn visit_expr_method_call(&mut self, node: &ExprMethodCall) {
        if let Some(access) = atomic_access_from_method_call(node) {
            self.accesses.push(access);
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn atomic_access_from_method_call(node: &ExprMethodCall) -> Option<AtomicAccess> {
    let ordering = match node.method.to_string().as_str() {
        "load" if node.args.len() == 1 => atomic_ordering(&node.args[0])?,
        "store" if node.args.len() == 2 => atomic_ordering(&node.args[1])?,
        "swap" | "fetch_add" | "fetch_sub" | "fetch_or" | "fetch_and" | "fetch_xor"
        | "fetch_max" | "fetch_min"
            if !node.args.is_empty() =>
        {
            atomic_ordering(node.args.last()?)?
        }
        "compare_exchange" | "compare_exchange_weak" if node.args.len() == 4 => {
            atomic_ordering(&node.args[2])?
        }
        "fetch_update" if node.args.len() == 3 => atomic_ordering(&node.args[0])?,
        _ => return None,
    };

    Some(AtomicAccess {
        key: receiver_key(&node.receiver),
        span: node.span(),
        ordering,
    })
}

fn receiver_key(expr: &Expr) -> String {
    expr.to_token_stream().to_string()
}

fn atomic_ordering(expr: &Expr) -> Option<AccessOrdering> {
    let Expr::Path(path) = expr else {
        return None;
    };

    match path.path.segments.last()?.ident.to_string().as_str() {
        "Relaxed" => Some(AccessOrdering::Relaxed),
        "Acquire" | "Release" | "AcqRel" | "SeqCst" => Some(AccessOrdering::Strong),
        _ => None,
    }
}

fn is_relaxed_ordering(expr: &Expr) -> bool {
    atomic_ordering(expr).is_some_and(|ordering| ordering == AccessOrdering::Relaxed)
}

fn block_contains_blocking_call(block: &Block) -> bool {
    let mut finder = BlockingCallFinder { found: false };
    finder.visit_block(block);
    finder.found
}

struct BlockingCallFinder {
    found: bool,
}

impl Visit<'_> for BlockingCallFinder {
    fn visit_expr_call(&mut self, node: &ExprCall) {
        if self.found {
            return;
        }
        if function_name(&node.func).is_some_and(|name| {
            matches!(
                name.as_str(),
                "spin_loop" | "yield_now" | "sleep" | "park" | "wait" | "wait_timeout"
            )
        }) {
            self.found = true;
            return;
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &ExprMethodCall) {
        if self.found {
            return;
        }
        if matches!(
            node.method.to_string().as_str(),
            "yield_now" | "sleep" | "wait" | "wait_timeout" | "park"
        ) {
            self.found = true;
            return;
        }
        visit::visit_expr_method_call(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(source: &str) -> Vec<Finding> {
        let syntax = syn::parse_file(source).unwrap();
        analyze_file(Path::new("test.rs"), source, &syntax)
    }

    #[test]
    fn reports_relaxed_wait_condition_in_wait_until() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicUsize, Ordering};

fn demo(wq: WaitQueue, counter: &AtomicUsize) {
    wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
}
"#,
        );

        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == Rule::WaitCondition)
        );
    }

    #[test]
    fn reports_relaxed_wait_condition_in_blocking_loop() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        core::hint::spin_loop();
    }
}
"#,
        );

        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == Rule::WaitCondition)
        );
    }

    #[test]
    fn reports_relaxed_publish_before_notify() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.notify_all(true);
}
"#,
        );

        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == Rule::PublishBeforeNotify)
        );
    }

    #[test]
    fn ignores_release_wait_conditions() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicUsize, Ordering};

fn demo(wq: WaitQueue, counter: &AtomicUsize) {
    wq.wait_until(|| counter.load(Ordering::Acquire) == 1);
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn respects_ignore_comment() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicUsize, Ordering};

fn demo(wq: WaitQueue, counter: &AtomicUsize) {
    // sync-lint: ignore suspicious_relaxed_wait_condition
    wq.wait_until(|| counter.load(Ordering::Relaxed) == 1);
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn reports_relaxed_mixed_ordering_for_sync_wait_variable() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| flag.load(Ordering::Acquire));
}
"#,
        );

        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == Rule::MixedOrdering)
        );
    }

    #[test]
    fn reports_relaxed_mixed_ordering_after_publish_notify() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Release);
    wq.notify_all(true);
    let _ = flag.load(Ordering::Relaxed);
}
"#,
        );

        assert!(
            findings
                .iter()
                .any(|finding| finding.rule == Rule::MixedOrdering)
        );
    }

    #[test]
    fn ignores_mixed_ordering_without_sync_intent() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicU64, Ordering};

struct PollFrequencyController {
    consecutive_idle: AtomicU64,
}

impl PollFrequencyController {
    fn current_interval(&self) -> u64 {
        self.consecutive_idle.load(Ordering::Relaxed)
    }

    fn on_event(&self) {
        self.consecutive_idle.store(0, Ordering::Release);
    }
}
"#,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn ignores_compare_exchange_failure_ordering() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool) {
    while flag.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }

    let _ = flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed);
}
"#,
        );

        assert!(findings.is_empty());
    }
}
