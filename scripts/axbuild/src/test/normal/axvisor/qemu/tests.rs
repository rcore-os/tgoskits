use std::{collections::BTreeMap, fs};

use ostool::run::qemu::QemuConfig;
use tempfile::tempdir;

use super::{
    super::host_checks::load_axvisor_http_probe_config, Axvisor, AxvisorQemuCase,
    PreparedAxvisorQemuCase, combine_results, plan_qemu_case_artifacts,
    preserve_qemu_build_artifact,
};
use crate::{
    context::{ResolvedAxvisorRequest, WorkspaceContext},
    execution::qemu as test_qemu,
};

#[test]
fn qemu_build_groups_preserve_distinct_executable_artifacts() {
    let root = tempdir().unwrap();
    let target = "aarch64-unknown-none-softfloat";
    let workspace = WorkspaceContext::discover(Some(&root.path().join("target"))).unwrap();
    let vm_a = root.path().join("vm-a.toml");
    let vm_c = root.path().join("vm-c.toml");
    fs::write(&vm_a, "[kernel]\nkernel_path = 'vm-a.bin'\n").unwrap();
    fs::write(&vm_c, "[kernel]\nkernel_path = 'vm-c.bin'\n").unwrap();
    let common = format!(
        "target = '{target}'\nfeatures = []\nlog = 'Info'\nrustflags = \
         ['--cfg=case_a']\n[env]\nINPUT_MODE = 'a'\nCARGO_ENCODED_RUSTFLAGS = '--cfg=base'\n"
    );
    let source_a = root.path().join("case-a.toml");
    let source_b = root.path().join("case-empty.toml");
    let source_c = root.path().join("case-c.toml");
    let vm_list = |path: &std::path::Path| {
        toml::Value::Array(vec![toml::Value::String(path.display().to_string())]).to_string()
    };
    fs::write(
        &source_a,
        format!("vm_configs = {}\n{common}", vm_list(&vm_a)),
    )
    .unwrap();
    fs::write(&source_b, format!("vm_configs = []\n{common}")).unwrap();
    fs::write(
        &source_c,
        format!(
            "target = '{target}'\nfeatures = ['ax-driver/virtio-net']\nlog = 'Debug'\nrustflags = \
             ['--cfg=case_c']\nmax_cpu_num = 2\nvm_configs = {}\n[runtime]\ntask_stack_size = \
             0x60000\nscheduler_tick_ms = 5\n[env]\nINPUT_MODE = 'c'\nCARGO_ENCODED_RUSTFLAGS = \
             '--cfg=base'\n",
            vm_list(&vm_c),
        ),
    )
    .unwrap();
    let mut config = crate::axvisor::config::AxvisorConfig::default();
    config.common.target = Some(target.to_string());
    let runtime_env = ax_config::RuntimeEnv::default();
    let runtime = ax_config::resolve_runtime(
        config.common.max_cpu_num,
        config.common.runtime,
        &runtime_env,
    )
    .unwrap();
    let mut request = ResolvedAxvisorRequest {
        source: crate::build::ConfigSource::Custom,
        package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
        axvisor_dir: root.path().join("os/axvisor"),
        arch: "aarch64".to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        config,
        runtime,
        runtime_env,
        environment_features: Vec::new(),
        qemu_config: None,
        uboot_config: None,
        vmconfigs: Vec::new(),
    };
    request.runtime_env = ax_config::RuntimeEnv {
        smp: Some("3".into()),
        task_stack_size: Some("0x50000".into()),
        scheduler_tick_ms: Some("7".into()),
    };
    request.environment_features = vec!["ax-driver/virtio-blk".into()];
    request.vmconfigs = vec![root.path().join("stale.toml")];
    let mut cache = BTreeMap::new();
    let mut cases = Vec::new();
    for (name, path) in [("a", &source_a), ("empty", &source_b), ("c", &source_c)] {
        let (selected, cargo) =
            Axvisor::qemu_case_build_context(&request, path, &mut cache, &workspace).unwrap();
        fs::remove_file(path).unwrap();
        let cached =
            Axvisor::qemu_case_build_context(&request, path, &mut cache, &workspace).unwrap();
        assert_eq!(cached, (selected.clone(), cargo.clone()));
        cases.push(PreparedAxvisorQemuCase {
            case: AxvisorQemuCase {
                selected_checks: None,
                case: crate::execution::case::TestQemuCase {
                    name: name.into(),
                    display_name: name.into(),
                    case_dir: root.path().join(name),
                    qemu_config_path: root.path().join(format!("qemu-{name}.toml")),
                    test_commands: vec![],
                    grouped_command_selection: Default::default(),
                    host_http_server: None,
                    subcases: vec![],
                    grouped_subcase_filter: None,
                    ltp_case_id: None,
                },
                build_group: name.into(),
                build_config_path: path.clone(),
            },
            qemu: QemuConfig::default(),
            request: selected,
            cargo,
        });
    }
    assert_eq!(cases[0].cargo.env["INPUT_MODE"], "a");
    assert_eq!(cases[0].cargo.env["AX_LOG"], "info");
    assert_eq!(cases[0].cargo.env["SMP"], "3");
    assert_eq!(cases[0].cargo.env["AX_TASK_STACK_SIZE"], "327680");
    assert_eq!(cases[0].cargo.env["AX_SCHEDULER_TICK_MS"], "7");
    assert!(cases[0].cargo.env["CARGO_ENCODED_RUSTFLAGS"].contains("--cfg=case_a"));
    assert_eq!(cases[2].cargo.env["INPUT_MODE"], "c");
    assert_eq!(cases[2].cargo.env["AX_LOG"], "debug");
    assert_eq!(cases[2].cargo.env["SMP"], "2");
    assert_eq!(cases[2].cargo.env["AX_TASK_STACK_SIZE"], "393216");
    assert_eq!(cases[2].cargo.env["AX_SCHEDULER_TICK_MS"], "5");
    assert!(cases[2].cargo.env["CARGO_ENCODED_RUSTFLAGS"].contains("--cfg=case_c"));
    for case in &cases {
        assert!(
            case.cargo
                .features
                .contains(&"ax-driver/virtio-blk".to_string())
        );
    }
    assert!(
        !cases[0]
            .cargo
            .features
            .contains(&"ax-std/virtio-net".to_string())
    );
    assert!(
        cases[2]
            .cargo
            .features
            .contains(&"ax-std/virtio-net".to_string())
    );
    assert!(
        cases[2]
            .request
            .config
            .common
            .features
            .contains(&"ax-driver/virtio-net".to_string())
    );
    let groups = test_qemu::prepare_case_build_groups(&cases, |path| {
        let case = cases
            .iter()
            .find(|case| case.case.build_config_path.as_path() == path)
            .unwrap();
        Ok((case.request.clone(), case.cargo.clone()))
    })
    .unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].group.cases.len(), 2);
    assert_eq!(groups[1].group.cases.len(), 1);
    assert_eq!(groups[0].cargo, cases[1].cargo);
    for (case, image) in [(&cases[0], "vm-a.bin"), (&cases[2], "vm-c.bin")] {
        let runtime = Axvisor::qemu_case_runtime_request(case, None, &workspace).unwrap();
        assert_eq!(runtime.vmconfigs.len(), 1);
        assert!(
            fs::read_to_string(&runtime.vmconfigs[0])
                .unwrap()
                .contains(image)
        );
    }
    assert!(
        Axvisor::qemu_case_runtime_request(&cases[1], None, &workspace)
            .unwrap()
            .vmconfigs
            .is_empty()
    );
    assert!(
        Axvisor::qemu_case_runtime_request(&cases[0], Some(&[]), &workspace)
            .unwrap()
            .vmconfigs
            .is_empty()
    );
    let build_output = root.path().join("target/release/axvisor");
    let artifact_directory = root.path().join("preserved");
    fs::create_dir_all(build_output.parent().unwrap()).unwrap();
    let mut artifacts = Vec::new();
    for index in 0..groups.len() {
        fs::write(&build_output, format!("kernel-{index}")).unwrap();
        artifacts
            .push(preserve_qemu_build_artifact(&build_output, &artifact_directory, index).unwrap());
    }
    assert_eq!(fs::read(&artifacts[0]).unwrap(), b"kernel-0");
    assert_eq!(fs::read(&artifacts[1]).unwrap(), b"kernel-1");
    let case_groups = groups
        .iter()
        .map(|group| group.group.cases.as_slice())
        .collect::<Vec<_>>();
    let plan = plan_qemu_case_artifacts(&case_groups, &artifacts, |case| case.qemu.to_bin).unwrap();
    assert_eq!(plan[0].build_artifact, plan[1].build_artifact);
    assert_ne!(plan[0].build_artifact, plan[2].build_artifact);
}

fn ok() -> anyhow::Result<()> {
    Ok(())
}

fn err(message: &str) -> anyhow::Result<()> {
    Err(anyhow::anyhow!("{message}"))
}

#[test]
fn qemu_error_wins_over_successful_probe() {
    // A fail_regex match, terminal timeout, or QEMU spawn/exit failure must
    // fail the run even when the HTTP probe passed.
    assert!(
        combine_results(
            err("Fail pattern matched '(?i)panic': panicked at ..."),
            true,
            Some(ok()),
        )
        .is_err()
    );
    assert!(combine_results(err("QEMU timeout"), true, Some(ok())).is_err());
    assert!(combine_results(err("failed to spawn qemu"), true, Some(ok())).is_err());
}

#[test]
fn probe_error_wins_on_clean_qemu_exit() {
    let verdict = combine_results(ok(), true, Some(err("probe: expected 200 got 404")));
    assert!(
        verdict
            .unwrap_err()
            .to_string()
            .contains("probe: expected 200")
    );
}

#[test]
fn missing_probe_verdict_on_clean_qemu_exit_fails() {
    let verdict = combine_results(ok(), true, None);
    assert!(verdict.unwrap_err().to_string().contains("no verdict"));
}

#[test]
fn non_probe_case_uses_qemu_result() {
    assert!(combine_results(ok(), false, None).is_ok());
    assert!(combine_results(err("boot failed"), false, None).is_err());
}

#[test]
fn probe_config_parses_with_serde_defaults() {
    let dir =
        std::env::temp_dir().join(format!("axvisor-probe-config-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let probe_path = dir.join("qemu-aarch64.toml");
    let plain_path = dir.join("plain.toml");
    std::fs::write(&probe_path, "[host_http_probe]\nguest_port = 8081\n").unwrap();
    std::fs::write(&plain_path, "args = []\n").unwrap();

    let config = load_axvisor_http_probe_config(&probe_path)
        .unwrap()
        .expect("host_http_probe present");
    assert_eq!(config.guest_port, 8081);
    assert_eq!(config.connect_timeout_secs, 120);
    assert_eq!(config.request_timeout_secs, 5);

    assert!(
        load_axvisor_http_probe_config(&plain_path)
            .unwrap()
            .is_none()
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
