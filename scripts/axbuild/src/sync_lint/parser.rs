use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread,
};

use anyhow::{Context, anyhow};
use proc_macro2::Span;
use quote::ToTokens;
use syn::{
    Block, Expr, ExprCall, ExprClosure, ExprForLoop, ExprMethodCall, ExprPath, ExprWhile, File,
    FnArg, Ident, ImplItemFn, ItemFn, ItemImpl, ItemMacro, Local, Member, Pat, Stmt, Type,
    spanned::Spanned,
    visit::{self, Visit},
};

use super::rules::{AccessOrdering, AccessSummary, AnalysisResult, AtomicAccess, Finding, Rule};

fn file_findings(path: &Path) -> anyhow::Result<Vec<Finding>> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let syntax = syn::parse_file(&source)
        .with_context(|| format!("failed to parse Rust file {}", path.display()))?;
    Ok(analyze_file(path, &source, &syntax).findings)
}

pub(super) fn files_findings(files: Vec<PathBuf>) -> anyhow::Result<Vec<Finding>> {
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

pub(super) fn analyze_file(path: &Path, source: &str, syntax: &File) -> AnalysisResult {
    let mut analyzer = Analyzer::new(path, source);
    analyzer.visit_file(syntax);
    analyzer.finish()
}

struct Analyzer<'a> {
    path: &'a Path,
    lines: Vec<&'a str>,
    result: AnalysisResult,
    bindings: BindingContext,
    impl_self_types: Vec<String>,
}

impl<'a> Analyzer<'a> {
    fn new(path: &'a Path, source: &'a str) -> Self {
        Self {
            path,
            lines: source.lines().collect(),
            result: AnalysisResult::default(),
            bindings: BindingContext::default(),
            impl_self_types: Vec::new(),
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

        self.result
            .findings
            .push(Finding::new(self.path, span, rule, message));
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
                && is_observer_event_expr(second)
            {
                self.result.sync_intent_keys.insert(access.key);
                if access.ordering == AccessOrdering::Relaxed {
                    self.report(
                        access.span,
                        Rule::PublishBeforeNotify,
                        "Relaxed atomic write is immediately followed by a wake/notify/scheduling \
                         event",
                    );
                }
            }
        }
    }
}

impl Visit<'_> for Analyzer<'_> {
    fn visit_item_fn(&mut self, node: &ItemFn) {
        self.bindings.push_scope();
        self.bindings.bind_fn_inputs(&node.sig.inputs, None);
        self.visit_block(&node.block);
        self.bindings.pop_scope();
    }

    fn visit_item_impl(&mut self, node: &ItemImpl) {
        self.impl_self_types.push(type_key(&node.self_ty));
        visit::visit_item_impl(self, node);
        self.impl_self_types.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &ImplItemFn) {
        let receiver_key = self.impl_self_types.last().cloned();
        self.bindings.push_scope();
        self.bindings
            .bind_fn_inputs(&node.sig.inputs, receiver_key.as_deref());
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
    scopes: Vec<HashMap<String, String>>,
    next_binding_id: usize,
}

impl BindingContext {
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_fn_inputs(
        &mut self,
        inputs: &syn::punctuated::Punctuated<FnArg, syn::token::Comma>,
        receiver_key: Option<&str>,
    ) {
        for input in inputs {
            match input {
                FnArg::Receiver(_) => self.bind_receiver(receiver_key),
                FnArg::Typed(pat_type) => self.bind_pat(&pat_type.pat),
            }
        }
    }

    fn bind_name(&mut self, name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            let id = self.next_binding_id;
            self.next_binding_id += 1;
            scope.insert(name.to_string(), format!("binding#{id}"));
        }
    }

    fn bind_receiver(&mut self, receiver_key: Option<&str>) {
        if let Some(scope) = self.scopes.last_mut() {
            let key = receiver_key
                .map(|receiver_key| format!("receiver:{receiver_key}"))
                .unwrap_or_else(|| {
                    let id = self.next_binding_id;
                    self.next_binding_id += 1;
                    format!("binding#{id}")
                });
            scope.insert("self".to_string(), key);
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

    fn resolve_ident(&self, ident: &Ident) -> Option<&str> {
        let name = ident.to_string();
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).map(String::as_str))
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

fn is_observer_event_expr(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(method) => is_observer_event_name(&method.method.to_string()),
        Expr::Call(call) => {
            function_name(&call.func).is_some_and(|name| is_observer_event_name(&name))
        }
        _ => false,
    }
}

fn is_observer_event_name(name: &str) -> bool {
    matches!(
        name,
        "notify_one"
            | "notify_all"
            | "notify_one_with"
            | "ax_wait_queue_wake"
            | "ax_wait_queue_wake_one_with"
            | "wake"
            | "wake_by_ref"
            | "wake_one"
            | "wake_all"
            | "wake_task"
            | "wake_task_from_timer"
            | "unblock_task"
            | "unpark"
            | "send_ipi"
            | "send_ipi_to_cpu"
            | "futex_wake"
            | "wake_robust_futex"
            | "send_signal_thread_inner"
            | "send_signal_to_thread"
            | "send_signal_to_process"
            | "send_signal_to_process_group"
    )
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
        && let Some(binding_key) = bindings.resolve_ident(&segment.ident)
    {
        return binding_key.to_string();
    }

    format!("path:{}", expr.path.to_token_stream())
}

fn type_key(ty: &Type) -> String {
    ty.to_token_stream().to_string()
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
