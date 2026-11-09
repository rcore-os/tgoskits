//! CPU batch selection over the shared normal ArceOS executor.
pub(crate) mod catalog;
mod listing;

use anyhow::Context;
pub(crate) use listing::{listed_cases, print_plan, render_listing};

use self::catalog::{Catalog, Mode, root};
use super::{qemu::prepare_qemu_cases, runner::run_prepared_qemu_groups, types::ArceosQemuCase};
use crate::{arceos::ArceOS, execution::case::TestQemuCase};

pub(crate) async fn run(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    selected: Option<&str>,
    allow_missing: bool,
) -> anyhow::Result<()> {
    let root = root(arceos.app.workspace_root());
    let catalog = Catalog::load(&root)?;
    let selections = match catalog.select(arch, selected) {
        Ok(selections) => selections,
        Err(_) if allow_missing => return Ok(()),
        Err(error) => return Err(error),
    };
    let cases = selections
        .iter()
        .map(|selection| {
            let batch = selection.batch;
            ArceosQemuCase {
                case: TestQemuCase {
                    name: selection.label(),
                    display_name: selection.label(),
                    case_dir: root.clone(),
                    qemu_config_path: root.join(&batch.qemu_config),
                    test_commands: Vec::new(),
                    grouped_command_selection: Default::default(),
                    host_http_server: None,
                    subcases: Vec::new(),
                    grouped_subcase_filter: None,
                    ltp_case_id: None,
                },
                build_group: selection.label(),
                build_config_path: root.join(&batch.build_config),
                package: batch.image.package().to_owned(),
                feature: Some(selection.id.to_owned()),
                features: selection.features(),
            }
        })
        .collect();
    let mut prepared = prepare_qemu_cases(arceos, target, cases, None).await?;
    for (case, selection) in prepared.iter_mut().zip(&selections) {
        case.cargo.env.extend(selection.build_env());
        if selection.batch.mode == Mode::Normal {
            crate::support::qemu_success::replace_configured_success_regex(
                &mut case.qemu,
                vec![format!(
                    r"(?m)^{}\s*$",
                    regex::escape(&selection.completion())
                )],
            );
        }
        let source = std::fs::read_to_string(&case.case.build_config_path)?;
        let build: toml::Value = toml::from_str(&source).context("CPU build configuration")?;
        anyhow::ensure!(
            build
                .get("freestanding")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
                == (selection.batch.image == catalog::Image::Freestanding),
            "CPU image and build runtime disagree"
        );
    }
    println!(
        "running arceos cpu tests: arch={arch} boots={}",
        prepared.len()
    );
    run_prepared_qemu_groups(arceos, "cpu", "arceos cpu", &prepared).await
}
