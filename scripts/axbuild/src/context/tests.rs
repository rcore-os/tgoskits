use super::*;

mod arceos;
mod axvisor;
mod board_request;
mod common;
mod snapshot;
mod starry;
mod workspace;

#[test]
fn raw_cargo_target_dir_before_rustc_args_is_rejected() {
    let cargo = Cargo {
        package: "kernel".to_string(),
        args: vec!["--target-dir=other".to_string()],
        ..Default::default()
    };

    let error = reject_raw_target_dir_args(&cargo).unwrap_err();
    assert!(error.to_string().contains("raw `--target-dir`"));
}

#[test]
fn rustc_target_dir_argument_after_separator_is_allowed() {
    let cargo = Cargo {
        package: "kernel".to_string(),
        args: vec!["--".to_string(), "--target-dir=guest-value".to_string()],
        ..Default::default()
    };

    reject_raw_target_dir_args(&cargo).unwrap();
}

#[cfg(unix)]
#[test]
fn build_prebuild_hook_does_not_inherit_cargo_env() {
    use std::os::unix::ffi::OsStrExt;

    const TEST_ENV: &str = "AXBUILD_TEST_CARGO_ENV_BOUNDARY";

    let _lock = common::ENV_LOCK.lock().unwrap();
    let original_smp = std::env::var_os("SMP");
    let original_test_env = std::env::var_os(TEST_ENV);
    let root = common::tempdir().unwrap();
    let mut app = common::test_app_context(root.path());
    let observed_env = root.path().join("pre-build-env.bin");
    let quote_shell_path =
        |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let command = format!(
        "printf '%s\\0%s\\0%s\\0%s' \"${{SMP+x}}\" \"${{SMP-}}\" \"${{{TEST_ENV}+x}}\" \
         \"${{{TEST_ENV}-}}\" > {}; exit 73",
        quote_shell_path(&observed_env)
    );
    let different_value = |original: Option<&OsStr>, value: &str| {
        if original == Some(OsStr::new(value)) {
            format!("{value}-override")
        } else {
            value.to_string()
        }
    };
    let cargo = Cargo {
        env: HashMap::from([
            (
                "SMP".to_string(),
                different_value(original_smp.as_deref(), "17"),
            ),
            (
                TEST_ENV.to_string(),
                different_value(original_test_env.as_deref(), "cargo-only-value"),
            ),
        ]),
        target: "x86_64-unknown-none".to_string(),
        package: "test-workspace".to_string(),
        disable_someboot_build_config: true,
        pre_build_cmds: vec![command],
        ..Default::default()
    };

    let mut expected_env = Vec::new();
    for (index, value) in [original_smp.as_deref(), original_test_env.as_deref()]
        .into_iter()
        .enumerate()
    {
        if value.is_some() {
            expected_env.push(b'x');
        }
        expected_env.push(0);
        if let Some(value) = value {
            expected_env.extend_from_slice(value.as_bytes());
        }
        if index == 0 {
            expected_env.push(0);
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(app.build(cargo, root.path().join("build.toml")))
        .err();
    let observed_env = std::fs::read(&observed_env);
    let final_smp = std::env::var_os("SMP");
    let final_test_env = std::env::var_os(TEST_ENV);
    drop(_lock);

    let error = error.expect("the pre-build hook's failure must propagate before Cargo starts");
    assert!(
        format!("{error:#}").contains("exit status: 73"),
        "the pre-build hook's failure must propagate before Cargo starts: {error:#}"
    );
    assert_eq!(observed_env.unwrap(), expected_env);
    assert_eq!(final_smp, original_smp);
    assert_eq!(final_test_env, original_test_env);
}
