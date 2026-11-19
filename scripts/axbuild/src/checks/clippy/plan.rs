//! Portable, revision-bound Clippy plans and strict per-check CI accounting.
use std::{collections::BTreeSet, fs, path::Path, process::Command, time::Instant};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    check::ClippyCheck,
    expand::expand_clippy_checks,
    partition::partition,
    runner::{ProcessCargoRunner, run_clippy_checks},
    selection::{skip_unsupported_packages, validate_package_coverage, workspace_packages},
};

const SCHEMA: u32 = 1;
const WORKSPACE: &str = "${workspace}";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanInput {
    revision: Option<String>,
    tracked_diff: String,
    rustc: String,
    configuration: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckPlan {
    schema_version: u32,
    input: PlanInput,
    checks: Vec<ClippyCheck>,
    shards: Vec<Vec<usize>>,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Passed,
    Failed,
    NotRun,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckResult {
    check_id: usize,
    status: CheckStatus,
    elapsed_seconds: f64,
    error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShardResult {
    schema_version: u32,
    plan_digest: String,
    shard: usize,
    checks: Vec<CheckResult>,
    elapsed_seconds: f64,
}

impl CheckPlan {
    pub(super) fn create(
        root: &Path,
        checks: Vec<ClippyCheck>,
        count: usize,
    ) -> anyhow::Result<Self> {
        let checks = relocate(&checks, &root.to_string_lossy(), WORKSPACE)?;
        let input = plan_input(root, &checks)?;
        let shards = partition(&checks, count)?;
        let digest = hash(&serde_json::to_vec(&(SCHEMA, &input, &checks, &shards))?);
        let plan = Self {
            schema_version: SCHEMA,
            input,
            checks,
            shards,
            digest,
        };
        plan.validate()?;
        Ok(plan)
    }

    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.schema_version == SCHEMA,
            "unsupported Clippy plan schema"
        );
        ensure!(
            hash(&serde_json::to_vec(&(
                SCHEMA,
                &self.input,
                &self.checks,
                &self.shards
            ))?) == self.digest,
            "Clippy plan digest mismatch"
        );
        ensure!(
            !self.checks.is_empty() && !self.shards.is_empty(),
            "empty Clippy plan"
        );
        let mut members = BTreeSet::new();
        for shard in &self.shards {
            ensure!(!shard.is_empty(), "empty Clippy shard");
            for &id in shard {
                ensure!(
                    id < self.checks.len() && members.insert(id),
                    "duplicate or unknown Clippy check ID"
                );
            }
        }
        ensure!(
            members.len() == self.checks.len(),
            "Clippy plan omits checks"
        );
        Ok(())
    }

    pub(super) fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": SCHEMA, "revision": self.input.revision,
            "digest": self.digest, "checks": self.checks.len(),
            "shards": self.shards.iter().enumerate().map(|(i, checks)|
                serde_json::json!({"id":i+1,"checks":checks.len()})
            ).collect::<Vec<_>>()
        })
    }
}

pub(super) fn run_saved_plan(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    ensure!(
        args.jobs == 1,
        "saved Clippy plans execute serially inside each CI shard"
    );
    let path = args
        .execute_plan
        .as_ref()
        .or(args.verify_results.as_ref())
        .context("missing plan")?;
    let plan: CheckPlan = read_json(path)?;
    plan.validate()?;
    let manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&manifest)?;
    let root = metadata.workspace_root.as_std_path();
    // Re-expand the local catalog before accepting data from another runner.
    // It must match the saved commands and build inputs exactly, not merely
    // claim the same number of checks.
    let discovered = workspace_packages(&metadata);
    let packages = skip_unsupported_packages(discovered.clone());
    let local = relocate(
        &expand_clippy_checks(&packages, &metadata)?,
        &root.to_string_lossy(),
        WORKSPACE,
    )?;
    validate_package_coverage(&discovered, &local)?;
    ensure!(
        local == plan.checks,
        "Clippy catalog differs from saved plan"
    );
    ensure!(
        plan_input(root, &local)? == plan.input,
        "Clippy revision, toolchain or input configuration changed"
    );
    if args.verify_results.is_some() {
        return verify_results(
            &plan,
            args.results_dir
                .as_deref()
                .context("--results-dir is required")?,
        );
    }
    let shard = args.shard.context("--shard is required")?;
    let output = args
        .result_file
        .as_deref()
        .context("--result-file is required")?;
    let ids = plan
        .shards
        .get(
            shard
                .checked_sub(1)
                .context("shard numbering starts at 1")?,
        )
        .context("unknown Clippy shard")?;
    let checks = relocate(&plan.checks, WORKSPACE, &root.to_string_lossy())?;
    let started = Instant::now();
    let mut result = ShardResult {
        schema_version: SCHEMA,
        plan_digest: plan.digest.clone(),
        shard,
        checks: ids
            .iter()
            .map(|&id| CheckResult {
                check_id: id,
                status: CheckStatus::NotRun,
                elapsed_seconds: 0.0,
                error: None,
            })
            .collect(),
        elapsed_seconds: 0.0,
    };
    let mut runner = ProcessCargoRunner;
    for item in &mut result.checks {
        println!(
            "CLIPPY_CHECK_START: shard={shard} check={} {}",
            item.check_id,
            checks[item.check_id].label()
        );
        let timer = Instant::now();
        let outcome = run_clippy_checks(
            &mut runner,
            root,
            metadata.target_directory.as_std_path(),
            &checks[item.check_id..=item.check_id],
        );
        item.elapsed_seconds = timer.elapsed().as_secs_f64();
        match outcome {
            Ok(report) => {
                ensure!(
                    report.passed_checks == 1,
                    "Clippy runner completed without a verdict"
                );
                item.status = CheckStatus::Passed;
            }
            Err(error) => {
                item.status = CheckStatus::Failed;
                item.error = Some(format!("{error:#}"));
            }
        }
        println!(
            "CLIPPY_CHECK_RESULT: shard={shard} check={} status={:?} elapsed_s={:.3}",
            item.check_id, item.status, item.elapsed_seconds
        );
        if matches!(item.status, CheckStatus::Failed) {
            break;
        }
    }
    result.elapsed_seconds = started.elapsed().as_secs_f64();
    write_json(output, &result)?;
    ensure!(
        result
            .checks
            .iter()
            .all(|r| matches!(r.status, CheckStatus::Passed)),
        "Clippy shard {shard} did not pass every planned check"
    );
    println!(
        "CLIPPY_SHARD_PASS: shard={shard} checks={} elapsed_s={:.3} digest={}",
        ids.len(),
        result.elapsed_seconds,
        plan.digest
    );
    Ok(())
}

fn verify_results(plan: &CheckPlan, directory: &Path) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for path in fs::read_dir(directory).context("read Clippy shard results")? {
        let path = path?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let result: ShardResult = read_json(&path)?;
        ensure!(
            result.schema_version == SCHEMA && result.plan_digest == plan.digest,
            "foreign Clippy result: {}",
            path.display()
        );
        ensure!(seen.insert(result.shard), "duplicate Clippy shard result");
        let ids = plan
            .shards
            .get(
                result
                    .shard
                    .checked_sub(1)
                    .context("invalid result shard")?,
            )
            .context("unknown result shard")?;
        ensure!(
            result
                .checks
                .iter()
                .map(|r| r.check_id)
                .eq(ids.iter().copied()),
            "Clippy shard result omitted or reordered checks"
        );
        ensure!(
            result
                .checks
                .iter()
                .all(|r| matches!(r.status, CheckStatus::Passed)),
            "Clippy shard {} failed or did not run",
            result.shard
        );
        println!(
            "Clippy shard {}: {} checks passed in {:.3}s",
            result.shard,
            ids.len(),
            result.elapsed_seconds
        );
    }
    ensure!(
        seen.len() == plan.shards.len(),
        "missing Clippy shard results: expected {}, got {}",
        plan.shards.len(),
        seen.len()
    );
    println!(
        "CLIPPY_ALL_SHARDS_PASS: checks={} shards={} digest={}",
        plan.checks.len(),
        plan.shards.len(),
        plan.digest
    );
    Ok(())
}

fn relocate(checks: &[ClippyCheck], from: &str, to: &str) -> anyhow::Result<Vec<ClippyCheck>> {
    // JSON-escape the path as well as its replacement so spaces, Windows
    // separators and quotes cannot turn path substitution into invalid JSON.
    let from = serde_json::to_string(from)?;
    let to = serde_json::to_string(to)?;
    let json =
        serde_json::to_string(checks)?.replace(&from[1..from.len() - 1], &to[1..to.len() - 1]);
    Ok(serde_json::from_str(&json)?)
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn plan_input(root: &Path, checks: &[ClippyCheck]) -> anyhow::Result<PlanInput> {
    let git = |args: &[&str]| -> anyhow::Result<Option<Vec<u8>>> {
        let output = Command::new("git")
            .arg("-c")
            .arg(format!("safe.directory={}", root.display()))
            .args(args)
            .current_dir(root)
            .output()?;
        Ok(output.status.success().then_some(output.stdout))
    };
    let revision =
        git(&["rev-parse", "HEAD"])?.map(|b| String::from_utf8_lossy(&b).trim().to_string());
    let tracked_diff = hash(&git(&["diff", "HEAD", "--"])?.unwrap_or_default());
    let rustc = Command::new("rustc")
        .arg("-vV")
        .current_dir(root)
        .output()
        .context("read Clippy toolchain")?;
    ensure!(rustc.status.success(), "rustc version query failed");
    let mut inputs = serde_json::to_vec(checks)?;
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "rust-toolchain",
        ".cargo/config.toml",
        ".cargo/config",
    ] {
        inputs.extend_from_slice(name.as_bytes());
        if root.join(name).is_file() {
            inputs.extend(fs::read(root.join(name))?);
        }
    }
    for name in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "SMP",
        "AX_TARGET",
        "AX_PLATFORM",
    ] {
        inputs.extend_from_slice(name.as_bytes());
        if let Some(value) = std::env::var_os(name) {
            inputs.extend_from_slice(value.to_string_lossy().as_bytes());
        }
        inputs.push(0);
    }
    Ok(PlanInput {
        revision,
        tracked_diff,
        rustc: String::from_utf8(rustc.stdout)?,
        configuration: hash(&inputs),
    })
}

pub(super) fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.persist(path)
        .with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{super::check::ClippyCheckKind, *};

    #[test]
    fn result_accounting_rejects_missing_duplicate_foreign_and_unfinished_work() {
        let checks = (0..3)
            .map(|n| ClippyCheck {
                package: format!("fixture-{n}"),
                kind: ClippyCheckKind::Base,
                target: None,
                env: Vec::new(),
            })
            .collect();
        let root = tempfile::tempdir().unwrap();
        let plan = CheckPlan::create(root.path(), checks, 2).unwrap();
        let output = tempfile::tempdir().unwrap();
        assert!(verify_results(&plan, output.path()).is_err());
        for (i, ids) in plan.shards.iter().enumerate() {
            let mut result = ShardResult {
                schema_version: SCHEMA,
                plan_digest: plan.digest.clone(),
                shard: i + 1,
                checks: ids
                    .iter()
                    .map(|&id| CheckResult {
                        check_id: id,
                        status: CheckStatus::Passed,
                        elapsed_seconds: 0.,
                        error: None,
                    })
                    .collect(),
                elapsed_seconds: 0.,
            };
            let path = output.path().join(format!("{}.json", i + 1));
            result.checks[0].status = CheckStatus::NotRun;
            write_json(&path, &result).unwrap();
            assert!(verify_results(&plan, output.path()).is_err());
            result.checks[0].status = CheckStatus::Passed;
            result.plan_digest = "foreign".into();
            write_json(&path, &result).unwrap();
            assert!(verify_results(&plan, output.path()).is_err());
            result.plan_digest = plan.digest.clone();
            write_json(&path, &result).unwrap();
        }
        verify_results(&plan, output.path()).unwrap();
        fs::copy(
            output.path().join("1.json"),
            output.path().join("duplicate.json"),
        )
        .unwrap();
        assert!(verify_results(&plan, output.path()).is_err());
        let mut broken = plan;
        broken.shards[0].clear();
        assert!(broken.validate().is_err());
    }

    #[test]
    fn workspace_relocation_preserves_check_arguments_and_environment() {
        let checks = vec![ClippyCheck {
            package: "sample".into(),
            kind: ClippyCheckKind::Base,
            target: None,
            env: vec![("ASSET".into(), "/tmp/root with space/config.toml".into())],
        }];
        let portable = relocate(&checks, "/tmp/root with space", WORKSPACE).unwrap();
        assert_eq!(portable[0].env[0].1, "${workspace}/config.toml");
        assert_eq!(
            relocate(&portable, WORKSPACE, "/tmp/root with space").unwrap(),
            checks
        );
    }
}
