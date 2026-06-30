use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, bail};

mod git;
mod parser;
mod rules;

#[cfg(test)]
mod tests;

pub(crate) fn run_sync_lint_command(args: &crate::SyncLintArgs) -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&workspace_manifest)
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let packages = git::workspace_packages(&metadata);
    let selection = git::select_sync_lint_files(&workspace_root, &packages, args.since.as_deref())?;

    match &selection {
        git::SyncLintSelection::All { reason } => {
            if let Some(reason) = reason {
                println!("sync-lint fell back to full workspace scan: {reason}");
            }
            println!(
                "running sync-lint for {} workspace package(s) from {}",
                packages.len(),
                workspace_root.display()
            );
        }
        git::SyncLintSelection::Files(files) => {
            println!(
                "running incremental sync-lint for {} changed Rust file(s) from {}",
                files.len(),
                workspace_root.display()
            );
        }
    }

    let files = match selection {
        git::SyncLintSelection::All { .. } => git::workspace_rust_source_files(&packages)?,
        git::SyncLintSelection::Files(files) => files,
    };
    let findings = parser::files_findings(files)?;

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
            .collect::<HashSet<PathBuf>>()
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
