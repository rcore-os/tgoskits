use std::{fs, path::Path};

use tempfile::tempdir;

use super::run_nixos_app;
use crate::starry::app::{ArgsAppQemu, test_support::write_case_file};

fn prepare_workspace(root: &Path) {
    write_case_file(root, "nixos", "qemu-x86_64.toml", "args = []\n");
    write_case_file(root, "nixos", "requires", "nix\n");
    let cases = root.join("nixos-tests/starryos/cases");
    fs::create_dir_all(&cases).unwrap();
    for name in ["boot", "function-forbidden", "service", "unsupported"] {
        fs::write(cases.join(format!("{name}.nix")), "{ kind = \"boot\"; }\n").unwrap();
    }
}

fn run_args() -> ArgsAppQemu {
    ArgsAppQemu {
        test_case: Some("nixos".to_string()),
        caps: vec!["nix".to_string()],
        ..Default::default()
    }
}

#[tokio::test]
async fn unsupported_arch_is_rejected_before_running_a_case() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    let args = ArgsAppQemu {
        arch: Some("aarch64".to_string()),
        ..run_args()
    };
    let mut requests = Vec::new();
    let result = run_nixos_app(root.path(), &args, async |request| {
        requests.push(request);
        Ok(())
    })
    .await;

    assert!(
        requests.is_empty(),
        "invalid architecture reached the runner"
    );
    assert!(result.unwrap_err().to_string().contains("aarch64"));
}

#[tokio::test]
async fn missing_capability_is_rejected_before_running_a_case() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    for caps in [vec![], vec!["board:OrangePi-5-Plus".to_string()]] {
        let args = ArgsAppQemu { caps, ..run_args() };
        let mut requests = Vec::new();
        let result = run_nixos_app(root.path(), &args, async |request| {
            requests.push(request);
            Ok(())
        })
        .await;

        assert!(requests.is_empty(), "missing capability reached the runner");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing required capabilities: nix")
        );
    }
}

#[tokio::test]
async fn all_cases_continue_after_failures_and_report_every_failed_case() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    let args = ArgsAppQemu {
        all_nixos_cases: true,
        ..run_args()
    };
    let mut visited = Vec::new();
    let result = run_nixos_app(root.path(), &args, async |request| {
        let name = request.test_case.unwrap();
        let fails = name == "function-forbidden" || name == "unsupported";
        visited.push(name);
        anyhow::ensure!(!fails, "contract failure");
        Ok(())
    })
    .await;

    assert_eq!(
        visited,
        ["boot", "function-forbidden", "service", "unsupported"]
    );
    let message = result.unwrap_err().to_string();
    assert!(message.contains("function-forbidden"));
    assert!(message.contains("unsupported"));
}

#[tokio::test]
async fn listing_needs_no_capability_and_never_runs_a_case() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    let args = ArgsAppQemu {
        list_nixos_cases: true,
        caps: vec![],
        ..run_args()
    };
    run_nixos_app(root.path(), &args, async |_| {
        panic!("listing must not start kernel preparation")
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn valid_default_and_explicit_selection_reach_the_runner() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    for (arch, case, expected) in [
        (None, None, "boot"),
        (Some("x86_64"), Some("service"), "service"),
    ] {
        let args = ArgsAppQemu {
            arch: arch.map(str::to_string),
            nixos_case: case.map(str::to_string),
            ..run_args()
        };
        let mut requests = Vec::new();
        run_nixos_app(root.path(), &args, async |request| {
            requests.push(request);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].arch.as_deref(), Some("x86_64"));
        assert_eq!(requests[0].test_case.as_deref(), Some(expected));
    }
}

#[tokio::test]
async fn all_cases_succeed_only_when_every_runner_result_succeeds() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    let args = ArgsAppQemu {
        all_nixos_cases: true,
        ..run_args()
    };
    let mut visited = Vec::new();
    run_nixos_app(root.path(), &args, async |request| {
        visited.push(request.test_case.unwrap());
        Ok(())
    })
    .await
    .unwrap();

    assert_eq!(
        visited,
        ["boot", "function-forbidden", "service", "unsupported"]
    );
}

#[tokio::test]
async fn single_case_preserves_its_failure() {
    let root = tempdir().unwrap();
    prepare_workspace(root.path());
    let error = run_nixos_app(root.path(), &run_args(), async |_| {
        anyhow::bail!("kernel preparation failed")
    })
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "kernel preparation failed");
}
