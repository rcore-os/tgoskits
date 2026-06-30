use std::{collections::BTreeSet, path::Path};

use anyhow::ensure;

use super::{
    args::ArgsAppQemu,
    discover_apps,
    discovery::{discover_apps_with_ignore, validate_case_name},
    qemu::qemu_app_supports_arch,
    types::{StarryAppCase, StarryAppKind},
};

pub(crate) fn print_apps(workspace_root: &Path, kind: Option<StarryAppKind>) -> anyhow::Result<()> {
    for app in filtered_apps(workspace_root, kind)? {
        let kind = match app.kind {
            StarryAppKind::Qemu => "qemu",
            StarryAppKind::Board => "board",
        };
        let prebuild = if app.prebuild_path.is_some() {
            " prebuild"
        } else {
            ""
        };
        println!("{kind}	{}{prebuild}", app.name);
    }
    Ok(())
}

pub(crate) fn selected_apps(
    workspace_root: &Path,
    args: &ArgsAppQemu,
    kind: StarryAppKind,
) -> anyhow::Result<Vec<StarryAppCase>> {
    ensure!(
        args.all ^ args.test_case.is_some(),
        "`starry app qemu` requires exactly one of --all or -t/--test-case"
    );

    let mut apps = if args.test_case.is_some() {
        discover_apps_with_ignore(workspace_root, false)?
    } else {
        discover_apps(workspace_root)?
    };
    apps.retain(|app| app.kind == kind);
    if args.all && args.qemu_config.is_none() {
        let arch = args.arch.as_deref().unwrap_or("x86_64");
        apps.retain(|app| app.kind != StarryAppKind::Qemu || qemu_app_supports_arch(app, arch));
    }
    if let Some(case_name) = args.test_case.as_deref() {
        let case_name = validate_case_name(case_name)?;
        apps.retain(|app| app.name == case_name);
        ensure!(
            !apps.is_empty(),
            "unknown or ignored Starry app case `{case_name}`"
        );
    }
    Ok(apps)
}

pub(crate) fn missing_caps(app: &StarryAppCase, caps: &[String]) -> Vec<String> {
    let caps = caps.iter().map(String::as_str).collect::<BTreeSet<_>>();
    app.requires
        .iter()
        .filter(|required| !caps.contains(required.as_str()))
        .cloned()
        .collect()
}

fn filtered_apps(
    workspace_root: &Path,
    kind: Option<StarryAppKind>,
) -> anyhow::Result<Vec<StarryAppCase>> {
    let mut apps = discover_apps(workspace_root)?;
    if let Some(kind) = kind {
        apps.retain(|app| app.kind == kind);
    }
    Ok(apps)
}

#[cfg(test)]
#[path = "tests/selection.rs"]
mod tests;
