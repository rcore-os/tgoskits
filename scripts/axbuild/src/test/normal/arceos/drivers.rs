pub(crate) mod catalog;
use std::path::Path;

use anyhow::Context;

use super::{qemu::prepare_qemu_cases, runner::run_prepared_qemu_groups, types::ArceosQemuCase};
use crate::{
    arceos::ArceOS,
    execution::{case::TestQemuCase, qemu::ListedQemuCase},
};

pub(crate) fn root(workspace: &Path) -> std::path::PathBuf {
    workspace.join("test-suit/arceos/drivers")
}
pub(crate) async fn run(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    requested: Option<&str>,
    allow_missing: bool,
) -> anyhow::Result<()> {
    let root = root(arceos.app.workspace_root());
    let catalog = catalog::Catalog::load(&root)?;
    let selected = match catalog.select(arch, requested) {
        Ok(selected) => selected,
        Err(_) if allow_missing => return Ok(()),
        Err(error) => return Err(error),
    };
    let cases = selected
        .iter()
        .map(|selection| ArceosQemuCase {
            case: TestQemuCase {
                name: format!("{}/{}", selection.id, selection.selection()),
                display_name: selection.id.into(),
                case_dir: root.clone(),
                qemu_config_path: root.join(&selection.batch.qemu_config),
                test_commands: Vec::new(),
                grouped_command_selection: Default::default(),
                host_http_server: None,
                subcases: Vec::new(),
                grouped_subcase_filter: None,
                ltp_case_id: None,
            },
            build_group: selection.id.into(),
            build_config_path: root.join(&selection.batch.build_config),
            package: "arceos-driver-tests".into(),
            feature: Some(selection.id.into()),
            features: selection.features(),
        })
        .collect();
    let mut prepared = prepare_qemu_cases(arceos, target, cases, None).await?;
    for (case, selection) in prepared.iter_mut().zip(&selected) {
        case.cargo.env.extend(selection.build_env());
        case.verify_pcm_output = selection.verifies_pcm();
        if selection.verifies_pcm() {
            let config: toml::Value =
                toml::from_str(&std::fs::read_to_string(&case.case.case.qemu_config_path)?)?;
            anyhow::ensure!(
                config.get("host_pcm_waveform").is_some(),
                "selected driver requires host PCM verification"
            );
        }
        crate::support::qemu_success::replace_configured_success_regex(
            &mut case.qemu,
            vec![format!(
                r"(?m)^{}\s*$",
                regex::escape(&selection.completion())
            )],
        );
    }
    println!(
        "drivers: arch={arch} boots={} tests={}",
        prepared.len(),
        selected.iter().map(|s| s.tests.len()).sum::<usize>()
    );
    run_prepared_qemu_groups(arceos, "drivers", "arceos drivers", &prepared).await
}
pub(crate) fn render_listing(
    arceos: &ArceOS,
    arch: Option<&str>,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    let catalog = catalog::Catalog::load(&root(arceos.app.workspace_root()))?;
    let arches: Vec<_> = arch.map(|arch| vec![arch.to_owned()]).unwrap_or_else(|| {
        catalog
            .batches
            .values()
            .map(|batch| batch.arch.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    });
    let mut text = String::new();
    for arch in arches {
        let selected = catalog.select(&arch, requested)?;
        text.push_str(&format!("drivers / {arch}: boots={}\n", selected.len()));
        for selection in selected {
            text.push_str(&format!(
                "  {}: tests={} boots=1\n",
                selection.id,
                selection.tests.len()
            ));
            for test in selection.tests {
                text.push_str(&format!("    {}: {}\n", test.id, test.name));
            }
        }
    }
    Ok(text)
}
pub(crate) fn listed_cases(
    arceos: &ArceOS,
    requested: Option<&str>,
) -> anyhow::Result<Vec<ListedQemuCase>> {
    let catalog = catalog::Catalog::load(&root(arceos.app.workspace_root()))?;
    let mut result = Vec::new();
    for arch in catalog
        .batches
        .values()
        .map(|b| b.arch.clone())
        .collect::<std::collections::BTreeSet<_>>()
    {
        for selected in catalog.select(&arch, requested)? {
            result.push(ListedQemuCase {
                name: format!("{} ({} tests; 1 boot)", selected.id, selected.tests.len()),
                archs: vec![arch.clone()],
            });
        }
    }
    Ok(result)
}
pub(crate) fn print_plan(
    arceos: &ArceOS,
    arch: &str,
    target: &str,
    requested: Option<&str>,
) -> anyhow::Result<()> {
    let root = root(arceos.app.workspace_root());
    let catalog = catalog::Catalog::load(&root)?;
    let selected = catalog.select(arch, requested)?;
    let mut rows = Vec::new();
    for selection in &selected {
        let path = root.join(&selection.batch.build_config);
        let config = crate::build::load_config_input::<crate::arceos::config::ArceosConfig>(
            &crate::context::workspace_root_path()?,
            Some(&path),
            None,
        )?
        .config;
        anyhow::ensure!(config.freestanding, "drivers require freestanding image");
        let runtime: toml::Value = toml::from_str(&std::fs::read_to_string(
            root.join(&selection.batch.qemu_config),
        )?)
        .context("drivers runtime")?;
        use sha2::{Digest, Sha256};
        let identity = serde_json::to_vec(&(
            arch,
            target,
            &config,
            selection.features(),
            selection.build_env(),
        ))?;
        let build_identity = format!("{:x}", Sha256::digest(identity));
        rows.push(serde_json::json!({"batch":selection.id,"build_identity":build_identity,"members":selection.members(),"features":selection.features(),"build_environment":selection.build_env(),"build_config":path,"qemu_config":root.join(&selection.batch.qemu_config),"build_configuration":config,"runtime_configuration":runtime,"effective_success_regex":[format!(r"(?m)^{}\s*$",regex::escape(&selection.completion()))],"completion":selection.completion(),"verify_pcm_waveform":selection.verifies_pcm()}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"arch":arch,"target":target,"planned_boots":rows.len(),"test_invocations":selected.iter().map(|s|s.tests.len()).sum::<usize>(),"runs":rows})
        )?
    );
    Ok(())
}
