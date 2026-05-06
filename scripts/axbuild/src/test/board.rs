use anyhow::{anyhow, bail};

pub(crate) trait BoardTestGroupInfo {
    fn name(&self) -> &str;
    fn board_name(&self) -> &str;
}

pub(crate) fn filter_board_test_groups<T: BoardTestGroupInfo>(
    mut groups: Vec<T>,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
    suite_name: &str,
    empty_message: impl FnOnce() -> String,
) -> anyhow::Result<Vec<T>> {
    groups.sort_by(|left, right| {
        left.name()
            .cmp(right.name())
            .then_with(|| left.board_name().cmp(right.board_name()))
    });

    if let Some(case_name) = selected_case {
        let available = available_values(groups.iter().map(BoardTestGroupInfo::name));
        groups.retain(|group| group.name() == case_name);
        if groups.is_empty() {
            return Err(anyhow!(
                "unsupported {suite_name} board test case `{case_name}`. Supported cases are: \
                 {available}",
            ));
        }
    }

    if let Some(board_name) = selected_board {
        let available = available_values(groups.iter().map(BoardTestGroupInfo::board_name));
        groups.retain(|group| group.board_name() == board_name);
        if groups.is_empty() {
            return Err(anyhow!(
                "unsupported {suite_name} board test board `{board_name}`. Supported boards are: \
                 {available}",
            ));
        }
    }

    if groups.is_empty() {
        bail!("{}", empty_message());
    }

    Ok(groups)
}

fn available_values<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let available = values
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    if available.is_empty() {
        "<none>".to_string()
    } else {
        available
    }
}

pub(crate) fn finalize_board_test_run(suite_name: &str, failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {suite_name} board test groups passed");
        Ok(())
    } else {
        bail!(
            "{suite_name} board tests failed for {} group(s): {}",
            failed.len(),
            failed.join(", ")
        )
    }
}
