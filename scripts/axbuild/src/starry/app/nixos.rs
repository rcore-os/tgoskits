use std::path::Path;

use anyhow::ensure;

use super::{ArgsAppQemu, StarryAppKind, missing_caps, selected_apps};
use crate::{
    starry::test::{ArgsTestNixos, supported_cases},
    test::qemu::QemuTestSummary,
};

pub(in crate::starry) async fn run_nixos_app(
    workspace: &Path,
    args: &ArgsAppQemu,
    mut run_case: impl AsyncFnMut(ArgsTestNixos) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let arch = args.arch.as_deref().unwrap_or("x86_64");
    ensure!(
        arch == "x86_64",
        "unsupported Starry nixosTest architecture `{arch}`; supported: x86_64"
    );
    let cases = supported_cases(workspace)?;
    if args.list_nixos_cases {
        for case in &cases {
            println!("{}\tarch={}\ttarget={}", case.name, case.arch, case.target);
        }
        return Ok(());
    }
    for app in selected_apps(workspace, args, StarryAppKind::Qemu)? {
        let missing = missing_caps(&app, &args.caps);
        ensure!(
            missing.is_empty(),
            "Starry app `{}` is missing required capabilities: {}",
            app.name,
            missing.join(", ")
        );
    }
    let selected = if args.all_nixos_cases {
        cases.into_iter().map(|case| case.name).collect()
    } else {
        vec![
            args.nixos_case
                .clone()
                .unwrap_or_else(|| "boot".to_string()),
        ]
    };
    let mut summary = QemuTestSummary::default();
    for case_name in selected {
        let result = run_case(ArgsTestNixos {
            arch: Some(arch.to_string()),
            test_case: Some(case_name.clone()),
            list: false,
        })
        .await;
        if !args.all_nixos_cases {
            return result;
        }
        match result {
            Ok(()) => summary.pass_with_detail(case_name, arch),
            Err(error) => summary.fail_with_detail(case_name, format!("{error:#}")),
        }
    }
    summary.finish_with_total_detail("Starry nixosTest", "case", None)
}

#[cfg(test)]
#[path = "tests/nixos.rs"]
mod tests;
