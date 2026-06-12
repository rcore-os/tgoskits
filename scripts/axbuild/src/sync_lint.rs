use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{Metadata, Package};
use proc_macro2::Span;
use quote::ToTokens;
use syn::{
    Block, Expr, ExprCall, ExprClosure, ExprForLoop, ExprMethodCall, ExprPath, ExprWhile, File,
    FnArg, Ident, ImplItemFn, ItemFn, ItemMacro, Local, Member, Pat, Stmt,
    spanned::Spanned,
    visit::{self, Visit},
};
use walkdir::WalkDir;

pub(crate) fn run_sync_lint_command(args: &crate::SyncLintArgs) -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&workspace_manifest)
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let packages = workspace_packages(&metadata);
    let selection = select_sync_lint_files(&workspace_root, &packages, args.since.as_deref())?;

    match &selection {
        SyncLintSelection::All { reason } => {
            if let Some(reason) = reason {
                println!("sync-lint fell back to full workspace scan: {reason}");
            }
            println!(
                "running sync-lint for {} workspace package(s) from {}",
                packages.len(),
                workspace_root.display()
            );
        }
        SyncLintSelection::Files(files) => {
            println!(
                "running incremental sync-lint for {} changed Rust file(s) from {}",
                files.len(),
                workspace_root.display()
            );
        }
    }

    let files = match selection {
        SyncLintSelection::All { .. } => workspace_rust_source_files(&packages)?,
        SyncLintSelection::Files(files) => files,
    };
    let findings = files_findings(files)?;

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncLintSelection {
    All { reason: Option<String> },
    Files(Vec<PathBuf>),
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

fn package_dir(package: &Package) -> anyhow::Result<PathBuf> {
    let package_dir = package
        .manifest_path
        .clone()
        .into_std_path_buf()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("invalid manifest path for package `{}`", package.name))?;
    Ok(package_dir)
}

fn workspace_rust_source_files(packages: &[Package]) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    for package in packages {
        files.extend(rust_source_files(&package_dir(package)?));
    }
    Ok(files.into_iter().collect())
}

fn select_sync_lint_files(
    workspace_root: &Path,
    packages: &[Package],
    since: Option<&str>,
) -> anyhow::Result<SyncLintSelection> {
    let Some(since) = since else {
        return Ok(SyncLintSelection::All { reason: None });
    };

    let changed_paths = match crate::support::git::changed_paths_since(workspace_root, since) {
        Ok(paths) => paths,
        Err(err) => {
            return Ok(SyncLintSelection::All {
                reason: Some(format!("failed to diff against `{since}`: {err:#}")),
            });
        }
    };

    select_sync_lint_files_for_paths(workspace_root, packages, changed_paths)
}

fn select_sync_lint_files_for_paths<I>(
    workspace_root: &Path,
    packages: &[Package],
    changed_paths: I,
) -> anyhow::Result<SyncLintSelection>
where
    I: IntoIterator<Item = PathBuf>,
{
    let package_dirs = workspace_package_dirs(workspace_root, packages)?;
    let mut files = BTreeSet::new();

    for path in changed_paths {
        let path = normalize_changed_path(&path)?;
        if path.as_os_str().is_empty() {
            continue;
        }
        if !path.extension().is_some_and(|ext| ext == "rs") {
            continue;
        }
        let Some(_package_dir) = package_dir_for_path(&package_dirs, &path) else {
            return Ok(SyncLintSelection::All {
                reason: Some(format!(
                    "changed Rust path `{}` is outside any workspace package",
                    path.display()
                )),
            });
        };
        let absolute = workspace_root.join(&path);
        if absolute.is_file() {
            files.insert(absolute);
        }
    }

    Ok(SyncLintSelection::Files(files.into_iter().collect()))
}

fn workspace_package_dirs(
    workspace_root: &Path,
    packages: &[Package],
) -> anyhow::Result<Vec<PathBuf>> {
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let mut dirs = packages
        .iter()
        .map(|package| {
            let manifest = package.manifest_path.clone().into_std_path_buf();
            let dir = manifest
                .parent()
                .ok_or_else(|| anyhow!("invalid manifest path for package `{}`", package.name))?;
            dir.strip_prefix(&workspace_root)
                .map(Path::to_path_buf)
                .with_context(|| {
                    format!(
                        "workspace package `{}` manifest {} is outside workspace root {}",
                        package.name,
                        manifest.display(),
                        workspace_root.display()
                    )
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    dirs.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| left.cmp(right))
    });
    Ok(dirs)
}

fn package_dir_for_path<'a>(package_dirs: &'a [PathBuf], path: &Path) -> Option<&'a Path> {
    package_dirs
        .iter()
        .find(|dir| path == dir.as_path() || path.starts_with(dir))
        .map(PathBuf::as_path)
}

fn normalize_changed_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        bail!(
            "git diff returned absolute path `{}`; expected workspace-relative path",
            path.display()
        );
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            _ => bail!("invalid changed path `{}`", path.display()),
        }
    }
    Ok(normalized)
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
    Ok(analyze_file(path, &source, &syntax).findings)
}

fn files_findings(files: Vec<PathBuf>) -> anyhow::Result<Vec<Finding>> {
    let parallelism = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(files.len());
    if parallelism <= 1 || files.len() <= 1 {
        return files_findings_sequential(files);
    }

    let chunk_size = files.len().div_ceil(parallelism);
    let chunks = files
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();

    let mut findings = thread::scope(|scope| {
        let handles = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    let mut findings = Vec::new();
                    for file in chunk {
                        findings.extend(file_findings(&file)?);
                    }
                    Ok::<_, anyhow::Error>(findings)
                })
            })
            .collect::<Vec<_>>();

        let mut findings = Vec::new();
        for handle in handles {
            findings.extend(
                handle
                    .join()
                    .map_err(|_| anyhow!("sync-lint worker thread panicked"))??,
            );
        }
        Ok::<_, anyhow::Error>(findings)
    })?;
    findings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.column.cmp(&right.column))
            .then_with(|| left.rule.label().cmp(right.rule.label()))
            .then_with(|| left.message.cmp(&right.message))
    });
    Ok(findings)
}

fn files_findings_sequential(files: Vec<PathBuf>) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for file in files {
        findings.extend(file_findings(&file)?);
    }
    Ok(findings)
}

fn analyze_file(path: &Path, source: &str, syntax: &File) -> AnalysisResult {
    let mut analyzer = Analyzer::new(path, source);
    analyzer.visit_file(syntax);
    analyzer.finish()
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

#[derive(Debug, Default)]
struct AnalysisResult {
    accesses: Vec<AtomicAccess>,
    sync_intent_keys: HashSet<String>,
    findings: Vec<Finding>,
}

struct Analyzer<'a> {
    path: &'a Path,
    lines: Vec<&'a str>,
    result: AnalysisResult,
    bindings: BindingContext,
}

impl<'a> Analyzer<'a> {
    fn new(path: &'a Path, source: &'a str) -> Self {
        Self {
            path,
            lines: source.lines().collect(),
            result: AnalysisResult::default(),
            bindings: BindingContext::default(),
        }
    }

    fn finish(mut self) -> AnalysisResult {
        let mut summaries: HashMap<String, AccessSummary> = HashMap::new();
        for access in &self.result.accesses {
            let summary = summaries.entry(access.key.clone()).or_default();
            match access.ordering {
                AccessOrdering::Relaxed => summary.has_relaxed = true,
                AccessOrdering::Strong => summary.has_strong = true,
            }
        }

        let spans = self
            .result
            .accesses
            .iter()
            .filter(|access| access.ordering == AccessOrdering::Relaxed)
            .filter(|access| self.result.sync_intent_keys.contains(&access.key))
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

        self.result
    }

    fn report(&mut self, span: Span, rule: Rule, message: &'static str) {
        let start = span.start();
        if self.is_ignored(rule, start.line) {
            return;
        }

        self.result.findings.push(Finding {
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
        for access in atomic_accesses_in_expr(expr, &self.bindings) {
            self.result.sync_intent_keys.insert(access.key);
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
            if let Some(access) = atomic_write_access(first, &self.bindings)
                && is_notify_expr(second)
            {
                self.result.sync_intent_keys.insert(access.key);
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
    fn visit_item_fn(&mut self, node: &ItemFn) {
        self.bindings.push_scope();
        self.bindings.bind_fn_inputs(&node.sig.inputs);
        self.visit_block(&node.block);
        self.bindings.pop_scope();
    }

    fn visit_impl_item_fn(&mut self, node: &ImplItemFn) {
        self.bindings.push_scope();
        self.bindings.bind_fn_inputs(&node.sig.inputs);
        self.visit_block(&node.block);
        self.bindings.pop_scope();
    }

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

    fn visit_expr_closure(&mut self, node: &ExprClosure) {
        self.bindings.push_scope();
        for input in &node.inputs {
            self.bindings.bind_pat(input);
        }
        visit::visit_expr(self, &node.body);
        self.bindings.pop_scope();
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
        if let Some(access) = atomic_access_from_method_call(node, &self.bindings) {
            self.result.accesses.push(access);
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

    fn visit_expr_for_loop(&mut self, node: &ExprForLoop) {
        self.visit_expr(&node.expr);
        self.bindings.push_scope();
        self.bindings.bind_pat(&node.pat);
        visit::visit_block(self, &node.body);
        self.bindings.pop_scope();
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
        self.bindings.push_scope();
        self.check_block_for_publish_before_notify(node);
        visit::visit_block(self, node);
        self.bindings.pop_scope();
    }

    fn visit_local(&mut self, node: &Local) {
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.bindings.bind_pat(&node.pat);
    }
}

#[derive(Debug, Default)]
struct BindingContext {
    scopes: Vec<HashMap<String, usize>>,
    next_binding_id: usize,
}

impl BindingContext {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_fn_inputs(&mut self, inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>) {
        for input in inputs {
            match input {
                FnArg::Receiver(_) => self.bind_name("self"),
                FnArg::Typed(pat_type) => self.bind_pat(&pat_type.pat),
            }
        }
    }

    fn bind_name(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            let id = self.next_binding_id;
            self.next_binding_id += 1;
            scope.insert(name.to_string(), id);
        }
    }

    fn bind_pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(pat_ident) => {
                self.bind_name(&pat_ident.ident.to_string());
                if let Some((_at, subpat)) = &pat_ident.subpat {
                    self.bind_pat(subpat);
                }
            }
            Pat::Or(pat_or) => {
                for case in &pat_or.cases {
                    self.bind_pat(case);
                }
            }
            Pat::Paren(pat_paren) => self.bind_pat(&pat_paren.pat),
            Pat::Reference(pat_reference) => self.bind_pat(&pat_reference.pat),
            Pat::Slice(pat_slice) => {
                for elem in &pat_slice.elems {
                    self.bind_pat(elem);
                }
            }
            Pat::Struct(pat_struct) => {
                for field in &pat_struct.fields {
                    self.bind_pat(&field.pat);
                }
            }
            Pat::Tuple(pat_tuple) => {
                for elem in &pat_tuple.elems {
                    self.bind_pat(elem);
                }
            }
            Pat::TupleStruct(pat_tuple_struct) => {
                for elem in &pat_tuple_struct.elems {
                    self.bind_pat(elem);
                }
            }
            Pat::Type(pat_type) => self.bind_pat(&pat_type.pat),
            _ => {}
        }
    }

    fn resolve_ident(&self, ident: &Ident) -> Option<usize> {
        let name = ident.to_string();
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
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

fn atomic_write_access(expr: &Expr, bindings: &BindingContext) -> Option<AtomicAccess> {
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
    atomic_access_from_method_call(method, bindings)
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

fn atomic_accesses_in_expr(expr: &Expr, bindings: &BindingContext) -> Vec<AtomicAccess> {
    let mut finder = AtomicAccessFinder {
        accesses: Vec::new(),
        bindings,
    };
    finder.visit_expr(expr);
    finder.accesses
}

struct AtomicAccessFinder<'a> {
    accesses: Vec<AtomicAccess>,
    bindings: &'a BindingContext,
}

impl Visit<'_> for AtomicAccessFinder<'_> {
    fn visit_expr_method_call(&mut self, node: &ExprMethodCall) {
        if let Some(access) = atomic_access_from_method_call(node, self.bindings) {
            self.accesses.push(access);
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn atomic_access_from_method_call(
    node: &ExprMethodCall,
    bindings: &BindingContext,
) -> Option<AtomicAccess> {
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
        key: receiver_key(&node.receiver, bindings),
        span: node.span(),
        ordering,
    })
}

fn receiver_key(expr: &Expr, bindings: &BindingContext) -> String {
    receiver_key_parts(expr, bindings).unwrap_or_else(|| format!("expr:{}", expr.to_token_stream()))
}

fn receiver_key_parts(expr: &Expr, bindings: &BindingContext) -> Option<String> {
    match expr {
        Expr::Field(field) => {
            let mut base = receiver_key_parts(&field.base, bindings)?;
            base.push('.');
            base.push_str(&member_key(&field.member));
            Some(base)
        }
        Expr::Index(index) => {
            let mut base = receiver_key_parts(&index.expr, bindings)?;
            base.push_str("[_]");
            Some(base)
        }
        Expr::Group(group) => receiver_key_parts(&group.expr, bindings),
        Expr::Paren(paren) => receiver_key_parts(&paren.expr, bindings),
        Expr::Reference(reference) => receiver_key_parts(&reference.expr, bindings),
        Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Deref(_)) => {
            receiver_key_parts(&unary.expr, bindings)
        }
        Expr::Path(path) => Some(path_key(path, bindings)),
        _ => None,
    }
}

fn path_key(expr: &ExprPath, bindings: &BindingContext) -> String {
    if expr.qself.is_none()
        && expr.path.leading_colon.is_none()
        && expr.path.segments.len() == 1
        && let Some(segment) = expr.path.segments.first()
        && let Some(binding_id) = bindings.resolve_ident(&segment.ident)
    {
        return format!("binding#{binding_id}");
    }

    format!("path:{}", expr.path.to_token_stream())
}

fn member_key(member: &Member) -> String {
    match member {
        Member::Named(ident) => ident.to_string(),
        Member::Unnamed(index) => index.index.to_string(),
    }
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

    fn package(root: &Path, name: &str) -> Package {
        let value = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": format!("{name} 0.1.0 (path+file://{}/crates/{name})", root.display()),
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": format!("{}/crates/{name}/src/lib.rs", root.display()),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": serde_json::Map::new(),
            "manifest_path": format!("{}/crates/{name}/Cargo.toml", root.display()),
            "metadata": null,
            "publish": null,
            "authors": [],
            "categories": [],
            "keywords": [],
            "readme": null,
            "repository": null,
            "homepage": null,
            "documentation": null,
            "edition": "2021",
            "links": null,
            "default_run": null,
            "rust_version": null
        });
        serde_json::from_value(value).unwrap()
    }

    fn findings(source: &str) -> Vec<Finding> {
        let syntax = syn::parse_file(source).unwrap();
        analyze_file(Path::new("test.rs"), source, &syntax).findings
    }

    #[test]
    fn incremental_selection_keeps_changed_rust_files() {
        let root = tempfile::tempdir().unwrap();
        let src_dir = root.path().join("crates/alpha/src");
        fs::create_dir_all(&src_dir).unwrap();
        let lib = src_dir.join("lib.rs");
        fs::write(&lib, "").unwrap();
        let packages = vec![package(root.path(), "alpha")];

        let selection = select_sync_lint_files_for_paths(
            root.path(),
            &packages,
            [PathBuf::from("crates/alpha/src/lib.rs")],
        )
        .unwrap();

        assert_eq!(selection, SyncLintSelection::Files(vec![lib]));
    }

    #[test]
    fn incremental_selection_skips_changed_non_rust_package_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/alpha/src")).unwrap();
        let packages = vec![package(root.path(), "alpha")];

        let selection = select_sync_lint_files_for_paths(
            root.path(),
            &packages,
            [PathBuf::from("crates/alpha/README.md")],
        )
        .unwrap();

        assert_eq!(selection, SyncLintSelection::Files(Vec::new()));
    }

    #[test]
    fn incremental_selection_skips_changed_non_rust_global_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/alpha/src")).unwrap();
        let packages = vec![package(root.path(), "alpha")];

        let selection =
            select_sync_lint_files_for_paths(root.path(), &packages, [PathBuf::from("Cargo.lock")])
                .unwrap();

        assert_eq!(selection, SyncLintSelection::Files(Vec::new()));
    }

    #[test]
    fn incremental_selection_falls_back_for_global_rust_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/alpha/src")).unwrap();
        fs::write(root.path().join("build.rs"), "").unwrap();
        let packages = vec![package(root.path(), "alpha")];

        let selection =
            select_sync_lint_files_for_paths(root.path(), &packages, [PathBuf::from("build.rs")])
                .unwrap();

        assert!(matches!(
            selection,
            SyncLintSelection::All { reason: Some(reason) } if reason.contains("build.rs")
        ));
    }

    #[test]
    fn workspace_source_files_are_deduplicated_for_nested_packages() {
        let root = tempfile::tempdir().unwrap();
        let alpha_src = root.path().join("crates/alpha/src");
        let beta_src = root.path().join("crates/alpha/beta/src");
        fs::create_dir_all(&alpha_src).unwrap();
        fs::create_dir_all(&beta_src).unwrap();
        let alpha_lib = alpha_src.join("lib.rs");
        let beta_lib = beta_src.join("lib.rs");
        fs::write(&alpha_lib, "").unwrap();
        fs::write(&beta_lib, "").unwrap();
        let packages = vec![
            package(root.path(), "alpha"),
            package(root.path(), "alpha/beta"),
        ];

        let files = workspace_rust_source_files(&packages).unwrap();

        assert_eq!(files, vec![beta_lib, alpha_lib]);
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
    fn reports_relaxed_mixed_ordering_for_parenthesized_receiver() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| (flag).load(Ordering::Acquire));
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
    fn ignores_mixed_ordering_for_different_function_bindings_with_same_name() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn sync_path(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| flag.load(Ordering::Acquire));
}

fn stats_path(flag: &AtomicBool) {
    let _ = flag.load(Ordering::Relaxed);
}
"#,
        );

        let mixed = findings
            .iter()
            .filter(|finding| finding.rule == Rule::MixedOrdering)
            .collect::<Vec<_>>();

        assert_eq!(mixed.len(), 1);
    }

    #[test]
    fn ignores_mixed_ordering_for_shadowed_binding_in_inner_scope() {
        let findings = findings(
            r#"
use core::sync::atomic::{AtomicBool, Ordering};

fn demo(flag: &AtomicBool, wq: WaitQueue) {
    flag.store(true, Ordering::Relaxed);
    wq.wait_until(|| flag.load(Ordering::Acquire));

    {
        let flag = AtomicBool::new(false);
        let _ = flag.load(Ordering::Relaxed);
    }
}
"#,
        );

        let mixed = findings
            .iter()
            .filter(|finding| finding.rule == Rule::MixedOrdering)
            .collect::<Vec<_>>();

        assert_eq!(mixed.len(), 1);
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
