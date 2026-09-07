use std::{
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
};

use tempfile::tempdir;

use super::{
    ArgsTestNixos,
    nixos::{
        NixosAction, OutputMode, configure_p1_build_info, ensure_success, nix_hash_command,
        nix_test_command, plan_nixos_action, validate_kernel, validate_nar_hash,
    },
};
use crate::starry::build::{LogLevel, StarryBuildInfo};

fn write_case(root: &Path, name: &str) {
    let cases = root.join("nixos-tests/starryos/cases");
    fs::create_dir_all(&cases).unwrap();
    fs::write(cases.join(format!("{name}.nix")), "{ kind = \"boot\"; }\n").unwrap();
}

fn case_names(cases: &[super::nixos::NixosCase]) -> Vec<&str> {
    cases.iter().map(|case| case.name.as_str()).collect()
}

#[test]
fn listing_is_side_effect_free_and_reports_discovered_stems() {
    let root = tempdir().unwrap();
    write_case(root.path(), "service");
    write_case(root.path(), "hello-tmpfiles");
    write_case(root.path(), "boot");

    let action = plan_nixos_action(
        root.path(),
        &ArgsTestNixos {
            list: true,
            ..Default::default()
        },
    )
    .unwrap();

    let NixosAction::List { cases } = action else {
        panic!("listing must not build or run a case");
    };
    assert_eq!(case_names(&cases), ["boot", "hello-tmpfiles", "service"]);
    assert!(cases.iter().all(|case| case.arch == "x86_64"));
    assert!(
        cases
            .iter()
            .all(|case| case.target == "x86_64-unknown-none")
    );
}

#[test]
fn listing_rejects_a_missing_cases_directory() {
    let error = plan_nixos_action(
        Path::new("/workspace/that-does-not-exist"),
        &ArgsTestNixos {
            list: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Starry nixosTest cases directory is missing")
    );
}

#[test]
fn listing_rejects_illegal_stems() {
    let root = tempdir().unwrap();
    write_case(root.path(), "boot");
    fs::write(
        root.path().join("nixos-tests/starryos/cases/Default.nix"),
        "{ kind = \"boot\"; }\n",
    )
    .unwrap();

    let error = plan_nixos_action(
        root.path(),
        &ArgsTestNixos {
            list: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("illegal Starry nixosTest case"));
    assert!(error.to_string().contains("Default.nix"));
}

#[test]
fn unknown_case_is_rejected_with_discovered_names() {
    let root = tempdir().unwrap();
    write_case(root.path(), "boot");
    write_case(root.path(), "service");

    let error = plan_nixos_action(
        root.path(),
        &ArgsTestNixos {
            arch: Some("x86_64".to_string()),
            test_case: Some("missing".to_string()),
            list: false,
        },
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("unsupported Starry nixosTest case `missing`"));
    assert!(message.contains("boot"));
    assert!(message.contains("service"));
}

#[test]
fn boot_selects_the_canonical_build_config() {
    let root = tempdir().unwrap();
    write_case(root.path(), "boot");
    let action = plan_nixos_action(
        root.path(),
        &ArgsTestNixos {
            arch: Some("x86_64".to_string()),
            test_case: Some("boot".to_string()),
            list: false,
        },
    )
    .unwrap();

    assert_eq!(
        action,
        NixosAction::Run {
            build_config: root
                .path()
                .join("apps/starry/nixos/build-x86_64-unknown-none.toml"),
            case_name: "boot".to_string(),
        }
    );
}

#[test]
fn discovered_cases_select_the_same_build_config() {
    let root = tempdir().unwrap();
    for case_name in ["service", "service-fail", "unsupported", "hello-tmpfiles"] {
        write_case(root.path(), case_name);
        let action = plan_nixos_action(
            root.path(),
            &ArgsTestNixos {
                arch: Some("x86_64".to_string()),
                test_case: Some(case_name.to_string()),
                list: false,
            },
        )
        .unwrap();
        assert_eq!(
            action,
            NixosAction::Run {
                build_config: root
                    .path()
                    .join("apps/starry/nixos/build-x86_64-unknown-none.toml"),
                case_name: case_name.to_string(),
            }
        );
    }
}

#[test]
fn p1_build_bounds_serial_logging_without_changing_capabilities() {
    let build_info = StarryBuildInfo {
        log: LogLevel::Info,
        features: vec!["nixos".to_string(), "ax-driver/nvme".to_string()],
        ..StarryBuildInfo::default()
    };

    let configured = configure_p1_build_info(build_info);

    assert_eq!(configured.log, LogLevel::Warn);
    assert_eq!(
        configured.features,
        ["nixos".to_string(), "ax-driver/nvme".to_string()]
    );
}

#[test]
fn missing_and_empty_kernels_are_rejected() {
    let root = tempdir().unwrap();
    let missing = root.path().join("missing.bin");
    assert!(validate_kernel(&missing).is_err());

    let empty = root.path().join("empty.bin");
    fs::write(&empty, []).unwrap();
    assert!(validate_kernel(&empty).is_err());
}

#[test]
fn valid_kernel_is_canonicalized() {
    let root = tempdir().unwrap();
    let kernel = root.path().join("starryos.bin");
    fs::write(&kernel, b"kernel").unwrap();

    assert_eq!(
        validate_kernel(&kernel).unwrap(),
        kernel.canonicalize().unwrap()
    );
}

#[test]
fn invalid_nar_hashes_are_rejected() {
    for hash in [
        "",
        "sha256:",
        "sha256-not-sri",
        "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
    ] {
        assert!(validate_nar_hash(hash).is_err(), "{hash:?} must fail");
    }
    assert!(validate_nar_hash("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_ok());
}

#[test]
fn nix_hash_command_is_exact_and_streams_output() {
    let kernel = Path::new("/tmp/starryos.bin");
    let command = nix_hash_command(kernel);

    assert_eq!(command.program, PathBuf::from("nix"));
    assert_eq!(
        command.args,
        [
            "hash",
            "path",
            "--type",
            "sha256",
            "--sri",
            "/tmp/starryos.bin"
        ]
    );
    assert_eq!(command.output, OutputMode::CaptureStdout);
}

#[test]
fn nix_test_command_is_exact_and_streams_driver_output() {
    let command = nix_test_command(
        Path::new("/workspace"),
        Path::new("/tmp/starryos.bin"),
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "service",
    );

    assert_eq!(command.program, PathBuf::from("nix"));
    assert_eq!(
        command.args,
        [
            "build",
            "--impure",
            "--print-build-logs",
            "--no-link",
            "--expr",
            "let testFlake = builtins.getFlake \"path:/workspace/nixos-tests/starryos\"; appFlake \
             = builtins.getFlake \"path:/workspace/apps/starry/nixos\"; system = \
             \"x86_64-linux\"; test = testFlake.lib.${system}.mkStarryNixosTest { kernelPath = \
             \"/tmp/starryos.bin\"; kernelNarHash = \
             \"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"; starryNixos = \
             appFlake.lib.${system}.starryNixos; caseName = \"service\"; }; in assert \
             testFlake.inputs.nixpkgs.outPath == appFlake.inputs.nixpkgs.outPath; builtins.trace \
             (\"[axbuild] Starry nixosTest kernel_store=\" + builtins.toString \
             test.kernelStorePath) (builtins.trace (\"[axbuild] Starry nixosTest \
             system_toplevel=\" + builtins.toString test.systemToplevel) test)",
        ]
    );
    assert_eq!(command.output, OutputMode::Inherit);
}

#[test]
fn process_failure_is_propagated() {
    let success = ExitStatus::from_raw(0);
    let failure = ExitStatus::from_raw(7 << 8);

    assert!(ensure_success(success, "nix test").is_ok());
    let error = ensure_success(failure, "nix test").unwrap_err();
    assert!(error.to_string().contains("status 7"));
}
