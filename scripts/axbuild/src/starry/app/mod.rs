mod args;
mod board;
mod board_assets;
mod build_config;
mod discovery;
mod linux_stage;
mod nixos;
mod qemu;
mod rootfs;
mod selection;
mod types;

pub use args::{AppCommand, ArgsApp, ArgsAppBoard, ArgsAppList, ArgsAppQemu};
pub(crate) use board::{configure_board_init_step, resolve_board_case};
pub(in crate::starry) use board_assets::prepare_app_board_session_assets;
pub(crate) use discovery::discover_apps;
pub(in crate::starry) use linux_stage::stage_in_default_linux;
pub(in crate::starry) use nixos::run_nixos_app;
pub(crate) use qemu::{app_qemu_test_case, prepare_qemu_app_case};
pub(crate) use selection::{missing_caps, print_apps, selected_apps};
pub use types::StarryAppKind;
pub(crate) use types::{StarryAppBoardCase, StarryAppCase, StarryAppQemuCase};

#[cfg(test)]
#[path = "tests/support.rs"]
mod test_support;

pub(in crate::starry) async fn run_qemu_apps(
    workspace: &std::path::Path,
    args: &ArgsAppQemu,
    mut run_app: impl AsyncFnMut(&StarryAppCase) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let apps = selected_apps(workspace, args, StarryAppKind::Qemu)?;
    let mut summary = crate::test::qemu::QemuTestSummary::default();
    let mut skipped = 0;
    for (index, app) in apps.iter().enumerate() {
        let missing = missing_caps(app, &args.caps);
        if !missing.is_empty() {
            if args.test_case.is_some() {
                anyhow::bail!(
                    "Starry app `{}` is missing required capabilities: {}",
                    app.name,
                    missing.join(", ")
                );
            }
            println!("SKIP\t{}\tmissing {}", app.name, missing.join(","));
            skipped += 1;
            continue;
        }
        println!(
            "{}",
            super::format_app_run_progress(index + 1, apps.len(), &app.name, args.arch.as_deref())
        );
        let result = run_app(app).await;
        if !args.all {
            return result;
        }
        match result {
            Ok(()) => summary.pass_with_detail(
                &app.name,
                args.arch
                    .as_deref()
                    .unwrap_or(crate::context::DEFAULT_STARRY_ARCH),
            ),
            Err(error) => {
                eprintln!("failed: {}: {error:#}", app.name);
                summary.fail_with_detail(&app.name, format!("{error:#}"));
            }
        }
    }
    anyhow::ensure!(apps.len() > skipped, "no Starry QEMU apps ran");
    summary.finish_with_total_detail(
        "Starry apps",
        "app",
        Some(&format!("selected={} skipped={skipped}", apps.len())),
    )
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{ArgsAppQemu, run_qemu_apps, test_support::write_case_file};

    #[tokio::test]
    async fn all_apps_continue_after_errors_and_preserve_aggregate_failure() {
        let root = tempdir().unwrap();
        for name in ["alpha", "beta", "gamma", "optional"] {
            write_case_file(root.path(), name, "qemu-x86_64.toml", "args = []\n");
        }
        write_case_file(root.path(), "optional", "requires", "optional-device\n");
        let args = ArgsAppQemu {
            all: true,
            arch: Some("x86_64".to_string()),
            ..Default::default()
        };
        for fail in [true, false] {
            let mut visited = Vec::new();
            let result = run_qemu_apps(root.path(), &args, async |app| {
                visited.push(app.name.clone());
                anyhow::ensure!(!fail || app.name == "gamma", "fixture execution failure");
                Ok(())
            })
            .await;
            assert_eq!(visited, ["alpha", "beta", "gamma"]);
            if fail {
                let message = result.unwrap_err().to_string();
                assert!(message.contains("alpha") && message.contains("beta"));
            } else {
                result.unwrap();
            }
        }

        for name in ["alpha", "beta", "gamma"] {
            write_case_file(root.path(), name, "requires", "optional-device\n");
        }
        let error = run_qemu_apps(root.path(), &args, async |_| {
            panic!("a batch without available capabilities must not execute apps")
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "no Starry QEMU apps ran");
    }

    #[tokio::test]
    async fn explicit_app_preserves_error_and_requires_its_capabilities() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "example", "qemu-x86_64.toml", "args = []\n");
        let args = ArgsAppQemu {
            test_case: Some("example".to_string()),
            arch: Some("x86_64".to_string()),
            ..Default::default()
        };
        let error = run_qemu_apps(root.path(), &args, async |_| {
            anyhow::bail!("original execution failure")
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "original execution failure");

        write_case_file(root.path(), "example", "requires", "device\n");
        let error = run_qemu_apps(root.path(), &args, async |_| {
            panic!("missing capability must not execute the app")
        })
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("missing required capabilities: device")
        );
    }
}
