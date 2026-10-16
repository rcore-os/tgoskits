//! Host command-line selection for lazily created builtin guests.

use alloc::vec::Vec;

use anyhow::{Result, bail};
use axvisor::builtin::BuiltinCatalog;

const SELECTOR_KEY: &str = "axvisor.vm_start";
const SELECTOR_PREFIX: &str = "axvisor.vm_start=";
/// Selects builtin IDs that are created at boot but started later on demand.
///
/// A guest prepared this way is created while the host filesystem still backs
/// its assets (before a passthrough guest forces that filesystem to be
/// released), and its start stays under shell/Web control.
const PREPARE_KEY: &str = "axvisor.vm_prepare";
const PREPARE_PREFIX: &str = "axvisor.vm_prepare=";

/// Immutable host startup selection parsed before Axvisor creates any VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupSelector {
    ids: Vec<usize>,
}

impl StartupSelector {
    /// Parses and validates the host selector against the builtin catalog.
    pub(crate) fn parse(command_line: Option<&str>, catalog: &BuiltinCatalog) -> Result<Self> {
        Ok(Self {
            ids: select_ids(SELECTOR_KEY, SELECTOR_PREFIX, command_line, |id| {
                catalog.get(id).is_some()
            })?,
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

/// The validated host startup selection: guests started at boot and guests
/// created at boot whose start is deferred to a later shell/Web command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StartupSelection {
    pub(crate) start: Vec<usize>,
    pub(crate) prepare: Vec<usize>,
}

/// Parses both host startup selectors and validates them together.
///
/// `known` reports whether an ID names a builtin guest. Every started and
/// prepared ID must be builtin, and the two selections must be disjoint.
pub(crate) fn startup_selection(
    command_line: Option<&str>,
    known: impl Fn(usize) -> bool,
) -> Result<StartupSelection> {
    let start = select_ids(SELECTOR_KEY, SELECTOR_PREFIX, command_line, &known)?;
    let prepare = select_ids(PREPARE_KEY, PREPARE_PREFIX, command_line, &known)?;
    for id in &prepare {
        if start.contains(id) {
            bail!("VM[{id}] is selected by both `{SELECTOR_KEY}` and `{PREPARE_KEY}`");
        }
    }
    Ok(StartupSelection { start, prepare })
}

fn select_ids(
    key: &str,
    prefix: &str,
    command_line: Option<&str>,
    known: impl Fn(usize) -> bool,
) -> Result<Vec<usize>> {
    let mut selected = None;
    for argument in ax_fs_ng::bootargs::tokens(command_line.unwrap_or(""))
        .into_iter()
        .take_while(|argument| argument != "--")
    {
        if argument == key || argument.starts_with(prefix) {
            let Some(value) = argument.strip_prefix(prefix) else {
                bail!("host command line parameter `{key}` requires a value");
            };
            if selected.is_some() {
                bail!("host command line parameter `{key}` appears more than once");
            }
            selected = Some(parse_ids(key, value, &known)?);
        }
    }
    Ok(selected.unwrap_or_default())
}

fn parse_ids(key: &str, value: &str, known: impl Fn(usize) -> bool) -> Result<Vec<usize>> {
    if value.trim().is_empty() {
        bail!("host command line parameter `{key}` has an empty value");
    }
    let mut ids = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            bail!("host command line parameter `{key}` contains an empty VM ID");
        }
        if !item.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("invalid VM ID `{item}` in `{key}`");
        }
        let id = item
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid VM ID `{item}` in `{key}`"))?;
        if !known(id) {
            bail!("VM[{id}] selected by `{key}` is not in the builtin catalog");
        }
        if ids.contains(&id) {
            bail!("VM[{id}] appears more than once in `{key}`");
        }
        ids.push(id);
    }
    Ok(ids)
}

#[cfg(any(test, axtest))]
mod tests {
    use super::{StartupSelector, parse_ids, select_ids, selected_vm_ids, startup_selection};
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
        let ids = parse_ids("axvisor.vm_start", "2,1", |id| [1, 2].contains(&id)).unwrap();
        assert_eq!(ids, [2, 1]);
        assert_eq!(
            select_ids(
                "axvisor.vm_start",
                "axvisor.vm_start=",
                Some("axvisor.vm_start=1 -- axvisor.vm_start=2"),
                |id| [1, 2].contains(&id)
            )
            .unwrap(),
            [1]
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn quoted_selector_is_tokenized_like_the_host_root_parameter() {
        assert_eq!(
            select_ids(
                "axvisor.vm_start",
                "axvisor.vm_start=",
                Some("root=/dev/vda axvisor.vm_start=\"2,1\""),
                |id| [1, 2].contains(&id)
            )
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
                parse_ids("axvisor.vm_start", value, |id| [1, 2].contains(&id)).is_err(),
                "{value}"
            );
        }
        assert!(
            select_ids(
                "axvisor.vm_start",
                "axvisor.vm_start=",
                Some("axvisor.vm_start"),
                |id| [1, 2].contains(&id)
            )
            .is_err()
        );
        assert!(
            select_ids(
                "axvisor.vm_start",
                "axvisor.vm_start=",
                Some("axvisor.vm_start=1 axvisor.vm_start=2"),
                |id| [1, 2].contains(&id)
            )
            .is_err()
        );
        assert!(
            select_ids(
                "axvisor.vm_start",
                "axvisor.vm_start=",
                Some("axvisor.vm_starting=1"),
                |id| [1, 2].contains(&id)
            )
            .unwrap()
            .is_empty()
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn prepare_selector_defers_start_without_a_start_selector() {
        let selection = startup_selection(Some("root=/dev/vda axvisor.vm_prepare=\"2,1\""), |id| {
            [1, 2].contains(&id)
        })
        .unwrap();
        assert!(selection.start.is_empty());
        assert_eq!(selection.prepare, [2, 1]);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn prepare_selector_coexists_with_the_start_selector() {
        let selection = startup_selection(Some("axvisor.vm_start=1 axvisor.vm_prepare=2"), |id| {
            [1, 2].contains(&id)
        })
        .unwrap();
        assert_eq!(selection.start, [1]);
        assert_eq!(selection.prepare, [2]);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn overlapping_start_and_prepare_selections_are_rejected() {
        let error = startup_selection(Some("axvisor.vm_start=1 axvisor.vm_prepare=1,2"), |id| {
            [1, 2].contains(&id)
        })
        .unwrap_err();
        assert!(error.to_string().contains("both"), "{error}");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn malformed_prepare_selector_values_are_rejected() {
        for line in [
            "axvisor.vm_prepare",
            "axvisor.vm_prepare=",
            "axvisor.vm_prepare=1,",
            "axvisor.vm_prepare=one",
            "axvisor.vm_prepare=1,1",
            "axvisor.vm_prepare=3",
            "axvisor.vm_prepare=1 axvisor.vm_prepare=2",
        ] {
            assert!(
                startup_selection(Some(line), |id| [1, 2].contains(&id)).is_err(),
                "{line}"
            );
        }
        assert!(
            startup_selection(Some("axvisor.vm_prepare=1 -- axvisor.vm_prepare=2"), |id| {
                [1, 2].contains(&id)
            })
            .unwrap()
            .prepare
                == [1]
        );
    }
}
