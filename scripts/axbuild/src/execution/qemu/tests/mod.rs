mod discovery;
mod rendering;

use super::{BuildConfigRef, prepare_case_build_groups};

#[test]
fn prepare_build_groups_reuses_equal_cargo_identity_across_runtime_configs() {
    use std::path::PathBuf;

    use ostool::build::config::Cargo;

    #[derive(Debug)]
    struct Case {
        build_group: String,
        build_config_path: PathBuf,
    }

    impl BuildConfigRef for Case {
        fn build_group(&self) -> &str {
            &self.build_group
        }

        fn build_config_path(&self) -> &std::path::Path {
            &self.build_config_path
        }
    }

    let cases = vec![
        Case {
            build_group: "direct-acpi".into(),
            build_config_path: "/tmp/direct/build.toml".into(),
        },
        Case {
            build_group: "ovmf-acpi".into(),
            build_config_path: "/tmp/ovmf/build.toml".into(),
        },
    ];
    let cargo = Cargo {
        package: "axvisor".into(),
        target: "x86_64-unknown-none".into(),
        ..Cargo::default()
    };

    let groups = prepare_case_build_groups(&cases, |_| Ok(((), cargo.clone())))
        .expect("equal Cargo identities share one build group");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].group.cases.len(), 2);
    assert_eq!(
        groups[0].group.cases[1].build_config_path,
        PathBuf::from("/tmp/ovmf/build.toml")
    );
}
