use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::Context;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::types::{
    CaseAssetConfig, CasePipeline, GROUPED_RUNNER_SCRIPT_FORMAT_VERSION, GroupedCaseRunnerConfig,
    PYTHON_PIPELINE_CACHE_VERSION, RUST_PIPELINE_CACHE_VERSION, TestQemuCase,
};

const CMAKE_TOOLCHAIN_TEMPLATE_PATH: &str = "src/test/cmake-toolchain.cmake.in";

pub(super) fn case_asset_cache_key(
    arch: &str,
    target: &str,
    pipeline: CasePipeline,
    case: &TestQemuCase,
    shared_rootfs: &Path,
    config: &CaseAssetConfig,
) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    hash_token(&mut hasher, "v3");
    hash_token(&mut hasher, arch);
    hash_token(&mut hasher, target);
    hash_token(&mut hasher, case.display_name.as_str());
    hash_token(&mut hasher, pipeline.as_str());
    for var in &config.cache_env_vars {
        hash_token(&mut hasher, var);
        hash_token(&mut hasher, std::env::var(var).unwrap_or_default().as_str());
    }
    // Only the C pipeline uses the CMake toolchain template; include it in the
    // key only when relevant so that changes to the template don't invalidate
    // caches for unrelated pipelines.
    if pipeline == CasePipeline::C {
        hash_file(
            &mut hasher,
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(CMAKE_TOOLCHAIN_TEMPLATE_PATH),
        )?;
    }
    if pipeline == CasePipeline::Python {
        hash_token(&mut hasher, PYTHON_PIPELINE_CACHE_VERSION);
    }
    if pipeline == CasePipeline::Rust {
        hash_token(&mut hasher, RUST_PIPELINE_CACHE_VERSION);
    }
    if pipeline == CasePipeline::Grouped {
        hash_grouped_runner_config(&mut hasher, &config.grouped_runner);
        hash_grouped_subcase_filter(&mut hasher, case.grouped_subcase_filter.as_ref());
    }

    hash_rootfs_fingerprint(&mut hasher, shared_rootfs)?;
    hash_tree(&mut hasher, &case.case_dir)?;
    if !case.qemu_config_path.starts_with(&case.case_dir) && case.qemu_config_path.is_file() {
        hash_file(&mut hasher, &case.qemu_config_path)?;
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_grouped_runner_config(hasher: &mut Sha256, config: &GroupedCaseRunnerConfig) {
    hash_token(hasher, GROUPED_RUNNER_SCRIPT_FORMAT_VERSION);
    hash_token(hasher, &config.runner_name);
    hash_token(hasher, &config.runner_path);
    match &config.autorun_profile_script {
        Some(script_name) => {
            hash_token(hasher, "autorun_profile_script");
            hash_token(hasher, script_name);
        }
        None => hash_token(hasher, "no_autorun_profile_script"),
    }
    hash_token(hasher, &config.begin_marker);
    hash_token(hasher, &config.passed_marker);
    hash_token(hasher, &config.failed_marker);
    hash_token(hasher, &config.all_passed_marker);
    hash_token(hasher, &config.all_failed_marker);
    hash_token(hasher, &config.success_regex);
    hash_token(hasher, &config.fail_regex);
}

fn hash_grouped_subcase_filter(hasher: &mut Sha256, filter: Option<&BTreeSet<String>>) {
    let Some(filter) = filter.filter(|filter| !filter.is_empty()) else {
        hash_token(hasher, "no_grouped_subcase_filter");
        return;
    };

    hash_token(hasher, "grouped_subcase_filter");
    for name in filter {
        hash_token(hasher, name);
    }
}

fn hash_tree(hasher: &mut Sha256, root: &Path) -> anyhow::Result<()> {
    let mut files = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to walk {}", root.display()))?;
    files.sort_by_key(|entry| entry.path().to_path_buf());

    for entry in files {
        let path = entry.path();
        if path == root || !entry.file_type().is_file() {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        hash_token(hasher, rel.to_string_lossy().as_ref());
        hash_file(hasher, path)?;
    }
    Ok(())
}

fn hash_rootfs_fingerprint(hasher: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    let len = metadata.len();
    hash_token(hasher, &len.to_string());

    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    hash_file_window(hasher, &mut file, 0, len)?;
    if len > 0 {
        hash_file_window(hasher, &mut file, len / 2, len)?;
        hash_file_window(hasher, &mut file, len.saturating_sub(1024 * 1024), len)?;
    }
    Ok(())
}

fn hash_file_window(
    hasher: &mut Sha256,
    file: &mut fs::File,
    offset: u64,
    file_len: u64,
) -> anyhow::Result<()> {
    let read_len = (file_len.saturating_sub(offset)).min(1024 * 1024);
    hash_token(hasher, &format!("{offset}:{read_len}"));
    file.seek(SeekFrom::Start(offset))
        .with_context(|| format!("failed to seek rootfs fingerprint window at offset {offset}"))?;

    let mut remaining = read_len;
    let mut buf = [0_u8; 8192];
    while remaining > 0 {
        let limit = remaining.min(buf.len() as u64) as usize;
        let read = file
            .read(&mut buf[..limit])
            .context("failed to read rootfs fingerprint window")?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        remaining -= read as u64;
    }
    Ok(())
}

fn hash_file(hasher: &mut Sha256, path: &Path) -> anyhow::Result<()> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut buf = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(())
}

fn hash_token(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value.as_bytes());
}
