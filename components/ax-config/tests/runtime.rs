use std::process::{Command, Output};

use ax_config::{
    CommonConfig, ConfigError, RuntimeEnv, RuntimeOptions, load_build_env, resolve_runtime,
};

#[test]
fn runtime_resolution_uses_only_each_winning_source() {
    let env = RuntimeEnv {
        smp: Some("0x_4".into()),
        task_stack_size: Some("0x_1_010".into()),
        scheduler_tick_ms: Some("2_0".into()),
    };
    let values = resolve_runtime(None, RuntimeOptions::default(), &env).unwrap();
    assert_eq!(values.cpu_capacity, 4);
    assert_eq!(values.task_stack_size, 0x1010);
    assert_eq!(values.scheduler_tick_interval_nanos().unwrap(), 20_000_000);

    let malformed = RuntimeEnv {
        smp: Some("not-a-number".into()),
        task_stack_size: Some("not-a-number".into()),
        scheduler_tick_ms: Some("not-a-number".into()),
    };
    let typed = RuntimeOptions {
        task_stack_size: Some(16),
        scheduler_tick_ms: Some(3),
    };
    let values = resolve_runtime(Some(2), typed, &malformed).unwrap();
    assert_eq!(values.cpu_capacity, 2);
    assert_eq!(values.task_stack_size, 16);
    assert_eq!(values.scheduler_tick_interval_nanos().unwrap(), 3_000_000);
    assert!(matches!(
        resolve_runtime(None, typed, &malformed),
        Err(ConfigError::Number { field: "SMP", .. })
    ));
    assert!(matches!(
        resolve_runtime(Some(2), RuntimeOptions::default(), &malformed),
        Err(ConfigError::Number {
            field: "AX_TASK_STACK_SIZE",
            ..
        })
    ));
    assert!(matches!(
        resolve_runtime(
            Some(2),
            RuntimeOptions {
                scheduler_tick_ms: None,
                ..typed
            },
            &malformed
        ),
        Err(ConfigError::Number {
            field: "AX_SCHEDULER_TICK_MS",
            ..
        })
    ));

    // A bad winning typed value must not fall back to an otherwise valid env.
    assert!(resolve_runtime(Some(0), typed, &env).is_err());
    assert!(
        resolve_runtime(
            Some(2),
            RuntimeOptions {
                task_stack_size: Some(0),
                ..typed
            },
            &env
        )
        .is_err()
    );
    assert!(
        resolve_runtime(
            Some(2),
            RuntimeOptions {
                scheduler_tick_ms: Some(0),
                ..typed
            },
            &env
        )
        .is_err()
    );
}

#[test]
fn runtime_validation_enforces_stack_layout_and_finite_tick_domain() {
    let empty = RuntimeEnv::default();
    for task_stack_size in [0, 17, usize::MAX & !15, (isize::MAX as usize) & !15] {
        let result = resolve_runtime(
            Some(1),
            RuntimeOptions {
                task_stack_size: Some(task_stack_size),
                scheduler_tick_ms: None,
            },
            &empty,
        );
        assert!(
            result.is_err(),
            "accepted invalid stack size {task_stack_size}"
        );
    }
    let max_tick = i64::MAX as u64 / 1_000_000;
    // Only the rounded allocation is a Layout. The additional virtual guard
    // range must fit usize, not an allocation with an extra physical page.
    let max_layout = (isize::MAX as usize) & !4095;
    assert!(
        resolve_runtime(
            None,
            RuntimeOptions {
                task_stack_size: Some(max_layout),
                scheduler_tick_ms: None
            },
            &empty,
        )
        .is_ok()
    );
    let values = resolve_runtime(
        Some(1),
        RuntimeOptions {
            task_stack_size: Some(4096 + 16),
            scheduler_tick_ms: Some(max_tick),
        },
        &empty,
    )
    .unwrap();
    assert_eq!(values.task_stack_size, 4112);
    assert_eq!(
        values.scheduler_tick_interval_nanos().unwrap(),
        max_tick * 1_000_000
    );
    for scheduler_tick_ms in [u64::MAX, 0, max_tick + 1] {
        let mut direct = values;
        direct.scheduler_tick_ms = scheduler_tick_ms;
        let error = direct.scheduler_tick_interval_nanos().unwrap_err();
        assert!(matches!(
            error,
            ConfigError::InvalidValue { field, value, .. }
                if field == "runtime.scheduler_tick_ms" && value == scheduler_tick_ms.to_string()
        ));
        assert!(
            resolve_runtime(
                None,
                RuntimeOptions {
                    task_stack_size: None,
                    scheduler_tick_ms: Some(scheduler_tick_ms)
                },
                &empty,
            )
            .is_err(),
            "accepted invalid tick {scheduler_tick_ms}"
        );
    }
    for smp in ["0", "-1", "0x", "18446744073709551616"] {
        assert!(
            resolve_runtime(
                None,
                RuntimeOptions::default(),
                &RuntimeEnv {
                    smp: Some(smp.into()),
                    ..empty.clone()
                },
            )
            .is_err(),
            "accepted invalid capacity {smp}"
        );
    }
}

#[test]
fn common_validation_is_pure_and_rejects_reserved_environment_inputs() {
    let mut common = CommonConfig::default();
    common.env.insert(
        "KERNEL_BUILTIN_CMDLINE".into(),
        "path with spaces = $HOME".into(),
    );
    common.validate().unwrap();
    for (key, replacement) in [
        ("SMP", "max_cpu_num"),
        ("AX_LOG", "log"),
        ("AX_TASK_STACK_SIZE", "runtime.task_stack_size"),
        ("AX_SCHEDULER_TICK_MS", "runtime.scheduler_tick_ms"),
    ] {
        common.env.insert(key.into(), "4".into());
        let error = common.validate().unwrap_err();
        assert!(matches!(
            &error, ConfigError::InvalidValue { field, .. } if field == &format!("env.{key}")
        ));
        assert!(error.to_string().contains(replacement));
        common.env.remove(key);
    }
    for (key, value) in [
        ("", "value"),
        ("BAD=KEY", "value"),
        ("BAD\0KEY", "value"),
        ("KEY", "bad\0value"),
    ] {
        common.env.insert(key.into(), value.into());
        assert!(common.validate().is_err());
        common.env.remove(key);
    }
    common.max_cpu_num = Some(0);
    assert!(common.validate().is_err());
    common.max_cpu_num = None;
    common.runtime.task_stack_size = Some(17);
    assert!(common.validate().is_err());
    common.runtime.task_stack_size = None;
    common.features.push("two features".into());
    assert!(common.validate().is_err());
    common.features.clear();
    common.target = Some(String::new());
    assert!(common.validate().is_err());
}

// Each branch runs in its own child process: no set_var, shared environment lock,
// or simulated build-script runtime is needed.
#[test]
fn environment_capture_and_build_helper_obey_the_process_protocol() {
    const CASE: &str = "AX_CONFIG_TEST_ENV_CASE";
    if let Ok(case) = std::env::var(CASE) {
        match case.as_str() {
            "valid" | "absent" => {
                let snapshot = RuntimeEnv::capture().unwrap();
                let expected = resolve_runtime(None, RuntimeOptions::default(), &snapshot).unwrap();
                let actual = load_build_env().unwrap();
                assert_eq!(actual, expected);
                assert_eq!(RuntimeEnv::capture().unwrap(), snapshot);
                if case == "valid" {
                    assert_eq!(actual.cpu_capacity, 3);
                    assert_eq!(actual.task_stack_size, 4112);
                    assert_eq!(actual.scheduler_tick_interval_nanos().unwrap(), 7_000_000);
                } else {
                    assert_eq!(actual.cpu_capacity, 16);
                    assert_eq!(actual.task_stack_size, 0x40000);
                    assert_eq!(actual.scheduler_tick_interval_nanos().unwrap(), 10_000_000);
                }
            }
            "invalid" => {
                let snapshot = RuntimeEnv::capture().unwrap();
                CommonConfig::default().validate().unwrap();
                assert!(
                    resolve_runtime(
                        Some(2),
                        RuntimeOptions {
                            task_stack_size: Some(16),
                            scheduler_tick_ms: Some(1)
                        },
                        &snapshot,
                    )
                    .is_ok()
                );
                assert!(matches!(
                    load_build_env(),
                    Err(ConfigError::Number { field: "SMP", .. })
                ));
            }
            "encoding" => {
                assert!(matches!(
                    RuntimeEnv::capture(),
                    Err(ConfigError::Encoding { field: "SMP", .. })
                ));
                assert!(matches!(
                    load_build_env(),
                    Err(ConfigError::Encoding { field: "SMP", .. })
                ));
            }
            _ => panic!("unknown child case"),
        }
        return;
    }
    for case in ["valid", "absent", "invalid"] {
        let mut command = child_command(case);
        if case == "valid" {
            command
                .env("SMP", "3")
                .env("AX_TASK_STACK_SIZE", "0x1010")
                .env("AX_SCHEDULER_TICK_MS", "7");
        } else if case == "invalid" {
            command
                .env("SMP", "oops")
                .env("AX_TASK_STACK_SIZE", "oops")
                .env("AX_SCHEDULER_TICK_MS", "oops");
        }
        let output = command.output().unwrap();
        check_child(&output);
        let stdout = String::from_utf8(output.stdout).unwrap();
        for key in ["SMP", "AX_TASK_STACK_SIZE", "AX_SCHEDULER_TICK_MS"] {
            assert!(
                stdout
                    .lines()
                    .any(|line| line == format!("cargo:rerun-if-env-changed={key}"))
            );
        }
    }
    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let output = child_command("encoding")
            .env("SMP", OsString::from_vec(vec![0xff]))
            .output()
            .unwrap();
        check_child(&output);
    }
}

fn child_command(case: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "environment_capture_and_build_helper_obey_the_process_protocol",
            "--nocapture",
        ])
        .env("AX_CONFIG_TEST_ENV_CASE", case)
        .env_remove("SMP")
        .env_remove("AX_TASK_STACK_SIZE")
        .env_remove("AX_SCHEDULER_TICK_MS");
    command
}

fn check_child(output: &Output) {
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}
