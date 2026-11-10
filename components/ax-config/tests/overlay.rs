#![cfg(feature = "toml")]

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use ax_config::{
    Board, CommonConfig, CommonDefaults, CommonPreset, ConfigError, LogLevel, RuntimeOptions,
    apply_overlay, parse_overlay,
};
use serde::{Deserialize, Serialize};

// A synthetic consumer exercises the documented one-level flatten boundary.
// Real OS-specific validation remains in axbuild rather than this crate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "schema", derive(ax_config::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(deny_unknown_fields))]
struct Config {
    #[serde(flatten)]
    common: CommonConfig,
    to_bin: bool,
    #[serde(rename = "app-c", skip_serializing_if = "Option::is_none")]
    app_c: Option<PathBuf>,
    vm_configs: Vec<PathBuf>,
    #[serde(skip_serializing)]
    input_only: Option<String>,
    #[serde(flatten, skip_serializing)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    extra: toml::Table,
}

impl Config {
    fn validate(&self, source: &Path) -> Result<(), ConfigError> {
        if let Some((field, _)) = self.extra.iter().next() {
            return Err(ConfigError::UnknownField {
                origin: Some(source.to_owned()),
                field: field.clone(),
                hint: match field.as_str() {
                    "std" => Some("select the appropriate Cargo features instead"),
                    "plat_dyn" => Some("dynamic platform selection is automatic"),
                    _ => None,
                },
                source: None,
            });
        }
        self.common.validate()
    }
}

fn base() -> Config {
    static BOARD: Board = Board {
        id: "synthetic-board",
        target: "synthetic-target",
    };
    let common = CommonPreset::new(
        &BOARD,
        CommonDefaults {
            log: LogLevel::Info,
            max_cpu_num: Some(4),
            runtime: RuntimeOptions {
                task_stack_size: Some(4112),
                scheduler_tick_ms: Some(5),
            },
        },
    )
    .with_feature_sets(&[&["alpha", "beta"], &["beta", "gamma"]])
    .with_rustflags(&["-Copt-level=2", "-Copt-level=2", "--cfg=with spaces"])
    .with_env(&[("KEEP", "first"), ("DELETE", "old"), ("KEEP", "last")])
    .materialize();
    Config {
        common,
        to_bin: true,
        app_c: Some("inherited/app".into()),
        vm_configs: vec!["vm.toml".into()],
        ..Config::default()
    }
}

#[test]
fn static_composition_and_sparse_overlay_follow_the_ordered_edit_contract() {
    let base = base();
    let source = Path::new("user/local.toml");
    let empty = parse_overlay(source, "").unwrap();
    let preserved = apply_overlay(&base, &empty, &["app-c"]).unwrap();
    preserved.validate(source).unwrap();
    assert_eq!(preserved.common.features, ["alpha", "beta", "gamma"]);
    assert_eq!(
        preserved.common.rustflags,
        ["-Copt-level=2", "-Copt-level=2", "--cfg=with spaces"]
    );
    assert_eq!(preserved.common.env["KEEP"], "last");

    let overlay = parse_overlay(
        source,
        r#"
preset = "synthetic"
unset = ["env.DELETE", "runtime.scheduler_tick_ms", "max_cpu_num", "app-c", "env.ABSENT"]
features = ["replacement", "duplicate", "duplicate"]
features_add = ["added", "added", "duplicate"]
features_remove = ["replacement", "not-present"]
rustflags = []
vm_configs = []
to_bin = false
[runtime]
[env]
NEW = "unaltered $HOME/path with spaces"
"#,
    )
    .unwrap();
    let config: Config = apply_overlay(&base, &overlay, &["app-c"]).unwrap();
    config.validate(source).unwrap();
    assert_eq!(config.common.target, base.common.target);
    assert_eq!(config.common.log, LogLevel::Info);
    assert_eq!(config.common.features, ["duplicate", "duplicate", "added"]);
    assert!(config.common.rustflags.is_empty());
    assert!(config.vm_configs.is_empty());
    assert!(!config.to_bin);
    assert_eq!(config.common.max_cpu_num, None);
    assert_eq!(config.common.runtime.task_stack_size, Some(4112));
    assert_eq!(config.common.runtime.scheduler_tick_ms, None);
    assert_eq!(config.app_c, None);
    assert_eq!(
        config.common.env,
        BTreeMap::from([
            ("KEEP".into(), "last".into()),
            ("NEW".into(), "unaltered $HOME/path with spaces".into()),
        ])
    );
    let exported = toml::to_string(&config).unwrap();
    let exported: toml::Table = toml::from_str(&exported).unwrap();
    for key in [
        "preset",
        "features_add",
        "features_remove",
        "unset",
        "max_cpu_num",
        "app-c",
    ] {
        assert!(!exported.contains_key(key));
    }

    let empty_tables = parse_overlay(source, "env = {}\nruntime = {}").unwrap();
    assert_eq!(apply_overlay(&base, &empty_tables, &[]).unwrap(), base);
    let cleared = parse_overlay(
        source,
        r#"unset = ["env", "runtime.task_stack_size"]
[env]
ONLY = "new"
[runtime]
task_stack_size = 8192"#,
    )
    .unwrap();
    let cleared: Config = apply_overlay(&base, &cleared, &[]).unwrap();
    assert_eq!(
        cleared.common.env,
        BTreeMap::from([("ONLY".into(), "new".into())])
    );
    assert_eq!(cleared.common.runtime.task_stack_size, Some(8192));
    assert_eq!(base.common.env["DELETE"], "old");
}

#[test]
fn explicit_field_origin_and_finite_clear_permissions_are_preserved() {
    let source = Path::new("somewhere/custom.toml");
    let inherited = parse_overlay(source, "to_bin = false").unwrap();
    assert!(!inherited.contains_field("app-c"));
    let explicit = parse_overlay(source, r#"app-c = "relative/app""#).unwrap();
    assert!(explicit.contains_field("app-c"));
    assert_eq!(explicit.source(), source);
    let config: Config = apply_overlay(&base(), &explicit, &["app-c"]).unwrap();
    assert_eq!(config.app_c.as_deref(), Some(Path::new("relative/app")));

    for path in [
        "target",
        "package",
        "runtime",
        "runtime.typo",
        "env.",
        "env.BAD=KEY",
        "vm_configs",
        "app-c",
        "fit_its",
    ] {
        let overlay = parse_overlay(source, &format!("unset = [{path:?}]")).unwrap();
        assert!(
            matches!(apply_overlay(&base(), &overlay, &[]),
                Err(ConfigError::InvalidValue { origin: Some(origin), field, .. })
                    if origin == source && field == "unset"
            ),
            "accepted unauthorized clear {path}"
        );
    }
    // Legal absent fields are no-ops, but an OS cannot grant arbitrary tree edits.
    let clear = parse_overlay(source, r#"unset = ["fit_its"]"#).unwrap();
    apply_overlay(&base(), &clear, &["fit_its"]).unwrap();
    let arbitrary = parse_overlay(source, r#"unset = ["to_bin"]"#).unwrap();
    assert!(apply_overlay(&base(), &arbitrary, &["to_bin"]).is_err());
    let conflict = parse_overlay(
        source,
        r#"features_add=["alpha"]
features_remove=["alpha"]"#,
    )
    .unwrap();
    assert!(matches!(apply_overlay(&base(), &conflict, &[]),
        Err(ConfigError::SelectorConflict { origin: Some(origin), .. }) if origin == source));
}

#[test]
fn parsing_and_final_validation_reject_wrong_types_unknown_keys_and_invalid_values() {
    use std::error::Error;

    let source = Path::new("broken.toml");
    let syntax = parse_overlay(source, "features = [").unwrap_err();
    match syntax {
        ConfigError::Syntax {
            origin,
            source: cause,
        } => {
            assert_eq!(origin, source);
            assert!(cause.span().is_some());
        }
        other => panic!("expected syntax failure, got {other}"),
    }
    for text in [
        r#"features_add = "alpha""#,
        r#"preset = 1"#,
        r#"unset = [1]"#,
    ] {
        let error = parse_overlay(source, text).unwrap_err();
        assert!(
            matches!(&error, ConfigError::Type { origin: Some(origin), .. } if origin == source)
        );
        assert!(error.source().is_some());
    }
    for text in [
        r#"max_cpu_num = "4""#,
        r#"to_bin = "false""#,
        r#"features = [1]"#,
        r#"log = "warn""#,
        "[runtime]\ntask_stack_size = \"4096\"",
        "[env]\nKEY = 1",
    ] {
        let overlay = parse_overlay(source, text).unwrap();
        let error = apply_overlay(&base(), &overlay, &[]).unwrap_err();
        assert!(
            matches!(&error, ConfigError::Type { origin: Some(origin), .. } if origin == source),
            "{text}: {error}"
        );
        assert!(error.source().is_some());
    }
    for text in [
        "[runtime]\nscheduler_tik_ms = 3",
        "[runtime]\nunknown = true",
    ] {
        let overlay = parse_overlay(source, text).unwrap();
        let error = apply_overlay(&base(), &overlay, &[]).unwrap_err();
        assert!(
            matches!(&error, ConfigError::UnknownField { origin: Some(origin), .. } if origin == source)
        );
        assert!(error.source().is_some());
    }
    for key in ["unknown", "std", "plat_dyn", "fit_its"] {
        let overlay = parse_overlay(source, &format!("{key} = true")).unwrap();
        let config: Config = apply_overlay(&base(), &overlay, &[]).unwrap();
        assert!(matches!(config.validate(source),
            Err(ConfigError::UnknownField { field, .. }) if field == key));
    }
    for text in [
        "max_cpu_num = 0",
        "[runtime]\ntask_stack_size = 17",
        "[runtime]\nscheduler_tick_ms = 0",
        "[env]\nSMP = \"4\"",
        "target = \"\"",
    ] {
        let overlay = parse_overlay(source, text).unwrap();
        let config: Config = apply_overlay(&base(), &overlay, &[]).unwrap();
        assert!(
            config.validate(source).is_err(),
            "accepted invalid input {text}"
        );
    }
    let reserved = parse_overlay(source, "features_add = [\"\"]").unwrap();
    assert!(apply_overlay(&base(), &reserved, &[]).is_err());
}

#[cfg(feature = "schema")]
#[test]
fn input_schema_uses_deserialization_and_shares_control_definitions_without_collisions() {
    let schema = ax_config::input_schema::<Config>().unwrap();
    let properties = schema.get("properties").unwrap().as_object().unwrap();
    for key in [
        "preset",
        "features_add",
        "features_remove",
        "unset",
        "target",
        "app-c",
        "input_only",
    ] {
        assert!(properties.contains_key(key), "missing input field {key}");
    }
    assert!(!properties.contains_key("extra"));
    assert_eq!(
        schema.get("additionalProperties").unwrap().as_bool(),
        Some(false)
    );
    if let Some(required) = schema.get("required") {
        for key in ["preset", "features_add", "features_remove", "unset"] {
            assert!(
                !required
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value.as_str() == Some(key))
            );
        }
    }
    let definitions = schema.get("$defs").unwrap().as_object().unwrap();
    let runtime = definitions.get("RuntimeOptions").unwrap();
    assert_eq!(
        runtime.get("additionalProperties").unwrap().as_bool(),
        Some(false)
    );
    let env = properties.get("env").unwrap();
    assert_eq!(
        env.get("additionalProperties")
            .unwrap()
            .get("type")
            .unwrap()
            .as_str(),
        Some("string")
    );

    #[derive(Serialize, ax_config::JsonSchema)]
    struct Collision {
        preset: Option<String>,
    }
    assert!(matches!(
        ax_config::input_schema::<Collision>(),
        Err(ConfigError::Schema { .. })
    ));
}
