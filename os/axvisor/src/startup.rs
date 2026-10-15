//! Host command-line selection for lazily created builtin guests.

use alloc::vec::Vec;

use anyhow::{Result, bail};
use axvisor::builtin::BuiltinCatalog;

const SELECTOR_KEY: &str = "axvisor.vm_start";
const SELECTOR_PREFIX: &str = "axvisor.vm_start=";

/// Immutable host startup selection parsed before Axvisor creates any VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupSelector {
    ids: Vec<usize>,
}

impl StartupSelector {
    /// Parses and validates the host selector against the builtin catalog.
    pub(crate) fn parse(command_line: Option<&str>, catalog: &BuiltinCatalog) -> Result<Self> {
        Ok(Self {
            ids: select_ids(command_line, |id| catalog.get(id).is_some())?,
        })
    }

    pub(crate) fn ids(&self) -> &[usize] {
        &self.ids
    }
}

/// Parses the host selector and validates every requested builtin VM ID.
///
/// The caller supplies the host boot arguments, not a guest `[kernel].cmdline`.
/// Linux-style tokenization is shared with root selection, and arguments after
/// `--` belong to the guest payload and are deliberately ignored.
pub(crate) fn selected_vm_ids(
    command_line: Option<&str>,
    catalog: &BuiltinCatalog,
) -> Result<Vec<usize>> {
    Ok(StartupSelector::parse(command_line, catalog)?
        .ids()
        .to_vec())
}

fn select_ids(command_line: Option<&str>, known: impl Fn(usize) -> bool) -> Result<Vec<usize>> {
    let mut selected = None;
    for argument in ax_fs_ng::bootargs::tokens(command_line.unwrap_or(""))
        .into_iter()
        .take_while(|argument| argument != "--")
    {
        if argument == SELECTOR_KEY || argument.starts_with(SELECTOR_PREFIX) {
            let Some(value) = argument.strip_prefix(SELECTOR_PREFIX) else {
                bail!("host command line parameter `{SELECTOR_KEY}` requires a value");
            };
            if selected.is_some() {
                bail!("host command line parameter `{SELECTOR_KEY}` appears more than once");
            }
            selected = Some(parse_ids(value, &known)?);
        }
    }
    Ok(selected.unwrap_or_default())
}

fn parse_ids(value: &str, known: impl Fn(usize) -> bool) -> Result<Vec<usize>> {
    if value.trim().is_empty() {
        bail!("host command line parameter `{SELECTOR_KEY}` has an empty value");
    }
    let mut ids = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            bail!("host command line parameter `{SELECTOR_KEY}` contains an empty VM ID");
        }
        if !item.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("invalid VM ID `{item}` in `{SELECTOR_KEY}`");
        }
        let id = item
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid VM ID `{item}` in `{SELECTOR_KEY}`"))?;
        if !known(id) {
            bail!("VM[{id}] selected by `{SELECTOR_KEY}` is not in the builtin catalog");
        }
        if ids.contains(&id) {
            bail!("VM[{id}] appears more than once in `{SELECTOR_KEY}`");
        }
        ids.push(id);
    }
    Ok(ids)
}

#[cfg(any(test, axtest))]
mod tests {
    use super::{StartupSelector, parse_ids, select_ids, selected_vm_ids};
    use axvisor::builtin::BuiltinCatalog;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn absent_selector_does_not_create_a_guest() {
        assert!(
            selected_vm_ids(Some("root=/dev/vda"), &BuiltinCatalog::default())
                .unwrap()
                .is_empty()
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn selector_is_an_immutable_empty_index_without_a_key() {
        let selector =
            StartupSelector::parse(Some("root=/dev/vda"), &BuiltinCatalog::default()).unwrap();
        assert!(selector.ids().is_empty());
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn selector_preserves_order_and_stops_at_guest_delimiter() {
        let ids = parse_ids("2,1", |id| [1, 2].contains(&id)).unwrap();
        assert_eq!(ids, [2, 1]);
        assert_eq!(
            select_ids(Some("axvisor.vm_start=1 -- axvisor.vm_start=2"), |id| [
                1, 2
            ]
            .contains(&id))
            .unwrap(),
            [1]
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn quoted_selector_is_tokenized_like_the_host_root_parameter() {
        assert_eq!(
            select_ids(Some("root=/dev/vda axvisor.vm_start=\"2,1\""), |id| [1, 2]
                .contains(&id))
            .unwrap(),
            [2, 1]
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn malformed_selector_values_are_rejected() {
        for value in [
            "",
            ",1",
            "1,",
            "1,,2",
            "one",
            "+1",
            "1,1",
            "3",
            "184467440737095516160",
        ] {
            assert!(
                parse_ids(value, |id| [1, 2].contains(&id)).is_err(),
                "{value}"
            );
        }
        assert!(select_ids(Some("axvisor.vm_start"), |id| [1, 2].contains(&id)).is_err());
        assert!(
            select_ids(Some("axvisor.vm_start=1 axvisor.vm_start=2"), |id| [1, 2]
                .contains(&id))
            .is_err()
        );
        assert!(
            select_ids(Some("axvisor.vm_starting=1"), |id| [1, 2].contains(&id))
                .unwrap()
                .is_empty()
        );
    }
}
