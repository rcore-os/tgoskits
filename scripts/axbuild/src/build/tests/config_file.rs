use ax_config::{RuntimeEnv, resolve_runtime};

use super::*;
use crate::{
    arceos::config::ArceosConfig, axvisor::config::AxvisorConfig, starry::config::StarryConfig,
};

#[test]
fn flattened_os_inputs_consume_shared_fields_and_reject_foreign_fields() {
    let shared = r#"
target = "x86_64-unknown-none"
features = ["net"]
features_add = ["fs"]
features_remove = ["net"]
rustflags = ["--cfg", 'label="two words"']
log = "Info"
max_cpu_num = 3
[runtime]
task_stack_size = 0x50000
scheduler_tick_ms = 5
[env]
CUSTOM = "literal ${value}"
"#;
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join("synthetic-config.toml");
    let source = source_path.as_path();
    let arceos: ArceosConfig =
        parse_build_config(source, &format!("package = 'synthetic-app'\n{shared}")).unwrap();
    let starry: StarryConfig = parse_build_config(source, shared).unwrap();
    let axvisor: AxvisorConfig = parse_build_config(source, shared).unwrap();
    arceos.validate().unwrap();
    starry.validate().unwrap();
    axvisor.validate().unwrap();
    for common in [&arceos.common, &starry.common, &axvisor.common] {
        let runtime =
            resolve_runtime(common.max_cpu_num, common.runtime, &RuntimeEnv::default()).unwrap();
        let cargo = into_base_cargo_config(
            common.clone(),
            "synthetic-app".into(),
            common.target.clone().unwrap(),
            vec![],
            runtime,
        );
        assert_eq!(cargo.features, ["fs"]);
        assert_eq!(cargo.env["CUSTOM"], "literal ${value}");
        assert_eq!(cargo.env["SMP"], "3");
        assert_eq!(cargo.env["AX_TASK_STACK_SIZE"], "327680");
        assert_eq!(cargo.env["AX_SCHEDULER_TICK_MS"], "5");
        assert_eq!(cargo.env["AX_LOG"], "info");
    }
    for invalid in [
        "typo = true",
        "std = true",
        "plat_dyn = true",
        "[runtime]\nsched_tick_ms = 3",
        "max_cpu_num = '4'",
        "preset = 'not-registered'",
        "features_add = ['fs']\nfeatures_remove = ['fs']",
    ] {
        assert!(
            parse_build_config::<ArceosConfig>(source, invalid).is_err(),
            "{invalid}"
        );
        assert!(
            parse_build_config::<StarryConfig>(source, invalid).is_err(),
            "{invalid}"
        );
        assert!(
            parse_build_config::<AxvisorConfig>(source, invalid).is_err(),
            "{invalid}"
        );
    }
    for invalid in ["package = 'other'", "app-c = 'app'"] {
        assert!(parse_build_config::<StarryConfig>(source, invalid).is_err());
        assert!(parse_build_config::<AxvisorConfig>(source, invalid).is_err());
    }
    assert!(parse_build_config::<ArceosConfig>(source, "vm_configs = []").is_err());
    assert!(parse_build_config::<StarryConfig>(source, "vm_configs = []").is_err());
    assert!(parse_build_config::<AxvisorConfig>(source, "fit_its = 'image.its'").is_err());
    for key in [
        "SMP",
        "AX_LOG",
        "AX_TASK_STACK_SIZE",
        "AX_SCHEDULER_TICK_MS",
    ] {
        let input = format!("target = 'x86_64-unknown-none'\n[env]\n{key} = '4'");
        let arceos: ArceosConfig =
            parse_build_config(source, &format!("package = 'synthetic-app'\n{input}")).unwrap();
        let starry: StarryConfig = parse_build_config(source, &input).unwrap();
        let axvisor: AxvisorConfig = parse_build_config(source, &input).unwrap();
        for error in [
            arceos.validate().unwrap_err(),
            starry.validate().unwrap_err(),
            axvisor.validate().unwrap_err(),
        ] {
            assert!(error.to_string().contains(&format!("env.{key}")), "{error}");
        }
    }
    for invalid in [
        "freestanding = true\napp-c = 'app'",
        "package = 'other'\napp-c = 'app'",
    ] {
        let parsed: ArceosConfig = parse_build_config(source, invalid).unwrap();
        assert!(parsed.validate().unwrap_err().to_string().contains("app-c"));
    }
    assert!(
        load_build_config::<ArceosConfig>(
            &tempfile::tempdir().unwrap().path().join("missing.toml"),
        )
        .is_err()
    );
}

#[test]
fn input_schema_matches_the_actual_overlay_boundary() {
    for schema in [
        ArceosConfig::input_schema().unwrap(),
        StarryConfig::input_schema().unwrap(),
        AxvisorConfig::input_schema().unwrap(),
    ] {
        let schema = serde_json::to_value(schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        let properties = schema["properties"].as_object().unwrap();
        for key in ["preset", "features_add", "features_remove", "unset"] {
            assert!(properties.contains_key(key), "{key}");
            assert!(
                !schema["required"]
                    .as_array()
                    .is_some_and(|keys| keys.iter().any(|item| item == key))
            );
        }
        assert!(!properties.contains_key("extra"));
        assert_eq!(properties["env"]["additionalProperties"]["type"], "string");
        let runtime = &schema["$defs"]["RuntimeOptions"];
        assert_eq!(runtime["additionalProperties"], false);
    }
}

#[test]
fn independent_cargo_environments_keep_runtime_fallback_local() {
    let first: StarryConfig = parse_build_config(
        Path::new("first.toml"),
        "max_cpu_num = 2\n[runtime]\ntask_stack_size = 0x50000\nscheduler_tick_ms = 5",
    )
    .unwrap();
    let second: StarryConfig = parse_build_config(Path::new("second.toml"), "").unwrap();
    let env = RuntimeEnv {
        smp: Some("4".into()),
        task_stack_size: Some("0x60000".into()),
        scheduler_tick_ms: Some("7".into()),
    };
    let first_runtime =
        resolve_runtime(first.common.max_cpu_num, first.common.runtime, &env).unwrap();
    let second_runtime =
        resolve_runtime(second.common.max_cpu_num, second.common.runtime, &env).unwrap();
    assert_eq!(second.common.max_cpu_num, None);
    let cargo = |common, runtime| {
        into_base_cargo_config(
            common,
            "synthetic".into(),
            "x86_64-unknown-none".into(),
            vec![],
            runtime,
        )
    };
    let first = cargo(first.common, first_runtime);
    let second = cargo(second.common, second_runtime);
    assert_eq!(
        (
            &first.env["SMP"],
            &first.env["AX_TASK_STACK_SIZE"],
            &first.env["AX_SCHEDULER_TICK_MS"]
        ),
        (&"2".to_string(), &"327680".to_string(), &"5".to_string())
    );
    assert_eq!(
        (
            &second.env["SMP"],
            &second.env["AX_TASK_STACK_SIZE"],
            &second.env["AX_SCHEDULER_TICK_MS"]
        ),
        (&"4".to_string(), &"393216".to_string(), &"7".to_string())
    );
    assert!(second.features.is_empty());
}
