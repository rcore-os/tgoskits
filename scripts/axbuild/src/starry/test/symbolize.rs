use std::{fs, io::Read, path::Path};

use anyhow::{Context, bail};

use crate::test::{case, case::TestQemuCase, host_http::HostHttpServerGuard};

pub(crate) fn start_qemu_case_host_http_server(
    case: &TestQemuCase,
) -> anyhow::Result<Option<HostHttpServerGuard>> {
    case.host_http_server
        .as_ref()
        .filter(|config| grouped_subcase_needs_host_http_server(case, config))
        .map(|config| HostHttpServerGuard::start(config, &case.name))
        .transpose()
}

fn grouped_subcase_needs_host_http_server(
    case: &TestQemuCase,
    config: &case::HostHttpServerConfig,
) -> bool {
    let Some(filter) = case
        .grouped_subcase_filter
        .as_ref()
        .filter(|filter| !filter.is_empty())
    else {
        return true;
    };

    case.subcases
        .iter()
        .filter(|subcase| filter.contains(subcase.name.as_str()))
        .any(|subcase| subcase_dir_references_host_http_server(&subcase.case_dir, config))
}

fn subcase_dir_references_host_http_server(
    subcase_dir: &Path,
    config: &case::HostHttpServerConfig,
) -> bool {
    if !subcase_dir.is_dir() {
        return false;
    }

    for entry in walkdir::WalkDir::new(subcase_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_scan_subcase_http_reference_path(entry.path()))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        if file_references_host_http_server(entry.path(), config) {
            return true;
        }
    }

    false
}

fn should_scan_subcase_http_reference_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    !matches!(
        name,
        ".git" | "build" | "target" | "CMakeFiles" | "__pycache__"
    )
}

fn file_references_host_http_server(path: &Path, config: &case::HostHttpServerConfig) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() > 1024 * 1024 {
        return false;
    }

    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return false;
    }

    let port = config.port.to_string();
    (content.contains("10.0.2.2") && content.contains(&port))
        || content.contains(&format!("http://{}:{port}", config.bind))
        || content.contains(&format!("http://localhost:{port}"))
        || content.contains(&format!("http://127.0.0.1:{port}"))
}

pub(crate) fn ensure_host_symbolize_output_matches(
    case_name: &str,
    outcome: crate::backtrace::SymbolizeAfterQemuOutcome,
    output: Option<&str>,
    regexes: &[String],
) -> anyhow::Result<()> {
    if outcome != crate::backtrace::SymbolizeAfterQemuOutcome::Symbolized {
        bail!("host backtrace symbolize did not run for Starry qemu case `{case_name}`");
    }
    let output =
        output.ok_or_else(|| anyhow::anyhow!("host backtrace symbolize produced no output"))?;
    for pattern in regexes {
        let regex = regex::Regex::new(pattern)
            .with_context(|| format!("invalid host_symbolize_success_regex `{pattern}`"))?;
        if !regex.is_match(output) {
            bail!(
                "host backtrace symbolize output for Starry qemu case `{case_name}` did not match \
                 `{pattern}`"
            );
        }
    }
    Ok(())
}
