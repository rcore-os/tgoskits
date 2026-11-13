use std::path::Path;

use anyhow::Context;
use sha2::{Digest, Sha256};

use super::{
    Bindings, Catalog, arceos_c_test_suit_build_config_path, arceos_c_test_suit_qemu_config_path,
    load_c_test_build_config,
};
use crate::{
    arceos::{ArceOS, cbuild},
    context::{BuildCliArgs, SnapshotPersistence},
};

pub(crate) fn render_listing(
    root: &Path,
    arch: Option<&str>,
    selected: Option<&str>,
) -> anyhow::Result<String> {
    let catalog = Catalog::load(root)?;
    let selections = catalog.select(selected)?;
    let tests: usize = selections
        .iter()
        .map(|selection| selection.tests.len())
        .sum();
    let mut output = if let Some(arch) = arch {
        format!("c / {arch}: boots={} tests={tests}\n", selections.len())
    } else {
        let arches = super::super::assets::arceos_test_suit_qemu_archs(root)?;
        format!(
            "c / {}: boots={} ({} per architecture) tests={tests} per architecture\n",
            arches.join(","),
            arches.len() * selections.len(),
            selections.len()
        )
    };
    for selection in selections {
        output.push_str(&format!(
            "  {}: {} ({} tests; 1 boot)\n",
            selection.id,
            selection.batch.name,
            selection.tests.len()
        ));
        for test in selection.tests {
            output.push_str(&format!("    {}: {}\n", test.id, test.name));
        }
    }
    Ok(output)
}
pub(crate) fn print_plan(
    arceos: &ArceOS,
    arch: &str,
    target: &str,
    selected: Option<&str>,
) -> anyhow::Result<()> {
    let root = super::super::assets::arceos_c_test_dir(arceos);
    let catalog = Catalog::load(&root)?;
    let selections = catalog.select(selected)?;
    let build_path = arceos_c_test_suit_build_config_path(&root, target)?;
    let qemu_path = arceos_c_test_suit_qemu_config_path(&root, arch)?;
    let build = load_c_test_build_config(&build_path)?;
    let runtime: toml::Value =
        toml::from_str(&std::fs::read_to_string(&qemu_path)?).context("C plan runtime")?;
    let mut runs = Vec::new();
    for selection in &selections {
        let bindings = Bindings::new(&catalog, selection);
        let request = arceos.prepare_request(
            BuildCliArgs {
                preset: None,
                config: Some(build_path.clone()),
                package: Some("ax-libc".into()),
                arch: None,
                target: Some(target.into()),
                smp: None,
                debug: false,
            },
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let mut features = request.config.common.features.clone();
        features.extend(selection.features());
        features.sort();
        features.dedup();
        let cargo = cbuild::prepare_c_app_cargo_config(
            &request,
            arceos.app.workspace_context(),
            &features,
        )?;
        let mut checks = super::load_c_test_qemu_config(&qemu_path)?;
        crate::support::qemu_success::replace_configured_success_regex(
            &mut checks,
            vec![format!(
                r"(?m)^{}\s*$",
                regex::escape(&selection.completion())
            )],
        );
        let identity = serde_json::json!({"arch":arch,"target":target,"build":build,"features":features,"cargo":cargo,"bindings":bindings.identity()});
        runs.push(serde_json::json!({"batch":selection.id,"members":selection.members(),"features":features,"cargo_features":cargo.features,"cargo_environment":cargo.env,"runtime_checks":checks,"build_identity":format!("{:x}",Sha256::digest(serde_json::to_vec(&identity)?)),"generated_identity":bindings.identity(),"completion":selection.completion(),"build_config":build_path,"qemu_config":qemu_path,"build_configuration":build,"runtime_configuration":runtime,"host_fixtures":if runtime.get("host_http_server").is_some(){vec!["http-server"]}else{vec![]}}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"arch":arch,"target":target,"planned_boots":selections.len(),"test_invocations":selections.iter().map(|s|s.tests.len()).sum::<usize>(),"runs":runs})
        )?
    );
    Ok(())
}
