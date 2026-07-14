pub(crate) fn parse_test_target(
    workspace_root: &Path,
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    let supported_targets = board::board_default_list(workspace_root)?
        .into_iter()
        .filter(|board| board.name.starts_with("qemu-"))
        .map(|board| board.target)
        .collect::<Vec<_>>();

    let supported_target_refs = supported_targets
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let supported_arches = supported_targets
        .iter()
        .map(|target| arch_for_target_checked(target))
        .collect::<anyhow::Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();

    let (arch, target) = resolve_starry_arch_and_target(arch.clone(), target.clone())?;
    validate_supported_target(&arch, "starry qemu tests", "arch values", &supported_arches)?;
    validate_supported_target(
        &target,
        "starry qemu tests",
        "targets",
        &supported_target_refs,
    )?;
    Ok((arch, target))
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = require_test_suite_dir(workspace_root)?;
    let selection = parse_starry_qemu_case_selection(selected_case);
    if let Some(direct_case) = selection.prefer_direct_case.as_deref()
        && direct_starry_qemu_case_exists(&test_suite_dir, direct_case)?
    {
        return load_qemu_cases_for_selection(
            &test_suite_dir,
            arch,
            target,
            Some(direct_case),
            None,
        );
    }

    load_qemu_cases_for_selection(
        &test_suite_dir,
        arch,
        target,
        selection.parent_case.as_deref(),
        selection.grouped_subcase_filter,
    )
}

fn load_qemu_cases_for_selection(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    grouped_subcase_filter: Option<BTreeSet<String>>,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    qemu_test::discover_qemu_cases(
        test_suite_dir,
        arch,
        target,
        selected_case,
        "Starry",
        "qemu",
    )?
    .into_iter()
    .map(|case| load_qemu_case(case, grouped_subcase_filter.clone()))
    .collect()
}

pub(crate) fn direct_starry_qemu_case_exists(
    test_suite_dir: &Path,
    selected_case: &str,
) -> anyhow::Result<bool> {
    match qemu_test::discover_all_qemu_cases(test_suite_dir, Some(selected_case), "Starry", "qemu")
    {
        Ok(cases) => Ok(!cases.is_empty()),
        Err(err) if err.kind() == qemu_test::ListQemuCasesErrorKind::UnknownSelectedCase => {
            Ok(false)
        }
        Err(err) => Err(anyhow::Error::new(err)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCaseSelection {
    pub(crate) parent_case: Option<String>,
    pub(crate) grouped_subcase_filter: Option<BTreeSet<String>>,
    pub(crate) prefer_direct_case: Option<String>,
}

pub(crate) fn parse_starry_qemu_case_selection(
    selected_case: Option<&str>,
) -> StarryQemuCaseSelection {
    let Some(selected_case) = selected_case else {
        return StarryQemuCaseSelection {
            parent_case: None,
            grouped_subcase_filter: None,
            prefer_direct_case: None,
        };
    };

    let parts = selected_case.split('/').collect::<Vec<_>>();
    let mapped = match parts.as_slice() {
        [group, subcase]
            if is_starry_qemu_system_group(group)
                && *subcase != "system"
                && !subcase.is_empty() =>
        {
            Some((
                format!("{group}/system"),
                *subcase,
                Some(selected_case.to_string()),
            ))
        }
        [group, "system", subcase] if is_starry_qemu_system_group(group) && !subcase.is_empty() => {
            Some((format!("{group}/system"), *subcase, None))
        }
        _ => None,
    };

    if let Some((parent_case, subcase, prefer_direct_case)) = mapped {
        return StarryQemuCaseSelection {
            parent_case: Some(parent_case),
            grouped_subcase_filter: Some(BTreeSet::from([subcase.to_string()])),
            prefer_direct_case,
        };
    }

    StarryQemuCaseSelection {
        parent_case: Some(selected_case.to_string()),
        grouped_subcase_filter: None,
        prefer_direct_case: None,
    }
}

fn is_starry_qemu_system_group(group: &str) -> bool {
    group == "qemu"
}

fn load_qemu_case(
    case: qemu_test::DiscoveredQemuCase,
    grouped_subcase_filter: Option<BTreeSet<String>>,
) -> anyhow::Result<StarryQemuCase> {
    let build_group = case.build_group;
    let build_config_path = case.build_config_path;
    let mut test_case = qemu_test::load_test_qemu_case_fields(
        case.display_name,
        case.name,
        case.case_dir,
        case.qemu_config_path,
        "Starry",
        true,
    )?;
    if let Some(filter) = grouped_subcase_filter.as_ref() {
        test_case.grouped_subcase_filter =
            Some(resolve_grouped_subcase_filter(&test_case, filter)?);
    }
    Ok(StarryQemuCase {
        case: test_case,
        build_group,
        build_config_path,
    })
}

fn resolve_grouped_subcase_filter(
    case: &TestQemuCase,
    filter: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    let canonical_names = case
        .subcases
        .iter()
        .map(|subcase| subcase.name.as_str())
        .collect::<BTreeSet<_>>();
    let aliases = grouped_subcase_selector_aliases(case)?;
    let mut resolved = BTreeSet::new();
    let mut missing = Vec::new();
    for requested in filter {
        if canonical_names.contains(requested.as_str()) {
            resolved.insert(requested.clone());
            continue;
        }

        match aliases.get(requested) {
            Some(matches) if matches.len() == 1 => {
                resolved.extend(matches.iter().cloned());
            }
            Some(matches) => bail!(
                "ambiguous Starry qemu grouped subcase selector `{}` for parent case `{}`; \
                 matches: {}",
                requested,
                case.display_name,
                matches.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
            None => missing.push(requested.as_str()),
        }
    }

    if missing.is_empty() {
        return Ok(resolved);
    }

    bail!(
        "unknown Starry qemu grouped subcase(s) {} for parent case `{}`",
        missing.join(", "),
        case.display_name
    )
}

fn grouped_subcase_selector_aliases(
    case: &TestQemuCase,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let mut aliases = BTreeMap::new();
    for subcase in &case.subcases {
        add_grouped_subcase_selector_alias(&mut aliases, &subcase.name, &subcase.name);
        for alias in grouped_subcase_binary_aliases(subcase)? {
            add_grouped_subcase_selector_alias(&mut aliases, &alias, &subcase.name);
        }
    }
    Ok(aliases)
}

fn add_grouped_subcase_selector_alias(
    aliases: &mut BTreeMap<String, BTreeSet<String>>,
    alias: &str,
    subcase_name: &str,
) {
    aliases
        .entry(alias.to_string())
        .or_default()
        .insert(subcase_name.to_string());
}

fn grouped_subcase_binary_aliases(
    subcase: &case::TestQemuSubcase,
) -> anyhow::Result<BTreeSet<String>> {
    let cmake_lists = subcase.case_dir.join("CMakeLists.txt");
    if !cmake_lists.is_file() {
        return Ok(BTreeSet::new());
    }

    let content = fs::read_to_string(&cmake_lists)
        .with_context(|| format!("failed to read {}", cmake_lists.display()))?;
    Ok(cmake_target_aliases(&content))
}

fn cmake_target_aliases(content: &str) -> BTreeSet<String> {
    let mut aliases = cmake_install_target_names(content);
    aliases.extend(cmake_executable_target_names(content));
    aliases
}

fn cmake_install_target_names(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in content.lines() {
        let tokens = cmake_line_tokens(line);
        if !tokens
            .first()
            .is_some_and(|token| token.eq_ignore_ascii_case("install"))
        {
            continue;
        }

        let mut collect_targets = false;
        for token in tokens.iter().skip(1) {
            let keyword = token.to_ascii_uppercase();
            if collect_targets {
                if cmake_install_target_boundary(&keyword) {
                    break;
                }
                names.insert(token.clone());
            } else if keyword == "TARGETS" {
                collect_targets = true;
            }
        }
    }
    names
}

fn cmake_executable_target_names(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in content.lines() {
        let tokens = cmake_line_tokens(line);
        if tokens
            .first()
            .is_some_and(|token| token.eq_ignore_ascii_case("add_executable"))
            && let Some(target) = tokens.get(1)
        {
            names.insert(target.clone());
        }
    }
    names
}

fn cmake_line_tokens(line: &str) -> Vec<String> {
    let line = line.split_once('#').map_or(line, |(code, _)| code);
    line.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '(' | ')'))
        .map(|token| token.trim_matches(|ch| matches!(ch, '"' | '\'')))
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn cmake_install_target_boundary(keyword: &str) -> bool {
    matches!(
        keyword,
        "ARCHIVE"
            | "BUNDLE"
            | "COMPONENT"
            | "CONFIGURATIONS"
            | "DESTINATION"
            | "EXCLUDE_FROM_ALL"
            | "EXPORT"
            | "FRAMEWORK"
            | "INCLUDES"
            | "LIBRARY"
            | "NAMELINK_COMPONENT"
            | "NAMELINK_ONLY"
            | "NAMELINK_SKIP"
            | "OBJECTS"
            | "OPTIONAL"
            | "PERMISSIONS"
            | "PRIVATE_HEADER"
            | "PUBLIC_HEADER"
            | "RENAME"
            | "RESOURCE"
            | "RUNTIME"
    )
}
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, bail};

use super::{StarryQemuCase, require_test_suite_dir};
use crate::{
    context::{arch_for_target_checked, resolve_starry_arch_and_target, validate_supported_target},
    starry::board,
    test::{case, case::TestQemuCase, qemu as qemu_test},
};
