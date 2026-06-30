use std::{
    env, fs,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};

pub(super) struct PerfOutputs {
    pub(super) work_dir: PathBuf,
    pub(super) dir: PathBuf,
    pub(super) raw: PathBuf,
    pub(super) folded: PathBuf,
    pub(super) flamegraph: PathBuf,
    pub(super) folded_boot: PathBuf,
    pub(super) flamegraph_boot: PathBuf,
    pub(super) folded_workload: PathBuf,
    pub(super) flamegraph_workload: PathBuf,
    pub(super) folded_post: PathBuf,
    pub(super) flamegraph_post: PathBuf,
    pub(super) folded_focus: PathBuf,
    pub(super) flamegraph_focus: PathBuf,
    pub(super) stack_depth_summary: PathBuf,
    pub(super) flamegraph_html: PathBuf,
    pub(super) summary: PathBuf,
    pub(super) qemu_config: PathBuf,
    pub(super) host_time: PathBuf,
    pub(super) host_perf: PathBuf,
    pub(super) resolve_stats: PathBuf,
    pub(super) window: PathBuf,
    pub(super) qmp_socket: PathBuf,
    pub(super) profile_stdout: PathBuf,
    pub(super) profile_stderr: PathBuf,
    pub(super) report_json: PathBuf,
    pub(super) report_md: PathBuf,
    pub(super) hotspots_csv: PathBuf,
    pub(super) hotspot_categories_csv: PathBuf,
}

pub(super) fn prepare_outputs(
    root: &Path,
    arch: &str,
    case: &str,
    out: Option<&Path>,
    output_dir: Option<&Path>,
) -> anyhow::Result<PerfOutputs> {
    let (work_dir, dir) = if let Some(out) = out {
        let dir = rooted_output_path(root, out);
        let work_dir = dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.clone());
        (work_dir, dir)
    } else {
        let output_root = output_dir
            .map(|path| rooted_output_path(root, path))
            .unwrap_or_else(|| root.join("target").join("qperf").join(case));
        let work_dir = output_root.join("perf").join(arch).join("latest");
        let dir = work_dir.join("qperf");
        (work_dir, dir)
    };
    if out.is_none() && work_dir.exists() {
        fs::remove_dir_all(&work_dir).with_context(|| {
            format!(
                "failed to remove previous qperf output directory {}",
                work_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create qperf output directory {}", dir.display()))?;
    fs::create_dir_all(&work_dir).with_context(|| {
        format!(
            "failed to create qperf work directory {}",
            work_dir.display()
        )
    })?;
    Ok(PerfOutputs {
        work_dir: work_dir.clone(),
        raw: dir.join("qperf.bin"),
        folded: dir.join("stack.folded"),
        flamegraph: dir.join("flamegraph.svg"),
        folded_boot: dir.join("stack.boot.folded"),
        flamegraph_boot: dir.join("flamegraph.boot.svg"),
        folded_workload: dir.join("stack.workload.folded"),
        flamegraph_workload: dir.join("flamegraph.workload.svg"),
        folded_post: dir.join("stack.post.folded"),
        flamegraph_post: dir.join("flamegraph.post.svg"),
        folded_focus: dir.join("stack.focus.folded"),
        flamegraph_focus: dir.join("flamegraph.focus.svg"),
        stack_depth_summary: dir.join("stack-depth-summary.csv"),
        flamegraph_html: dir.join("flamegraph.html"),
        summary: dir.join("summary.txt"),
        qemu_config: dir.join("qemu.toml"),
        host_time: dir.join("qemu.time.txt"),
        host_perf: dir.join("qemu.perf.csv"),
        resolve_stats: dir.join("resolve.stats.json"),
        window: dir.join("window.json"),
        qmp_socket: short_qmp_socket_path(),
        profile_stdout: work_dir.join("profile.stdout"),
        profile_stderr: work_dir.join("profile.stderr"),
        report_json: work_dir.join("report.json"),
        report_md: work_dir.join("report.md"),
        hotspots_csv: work_dir.join("hotspots.csv"),
        hotspot_categories_csv: work_dir.join("hotspot_categories.csv"),
        dir,
    })
}

fn short_qmp_socket_path() -> PathBuf {
    let base = if Path::new("/tmp").is_dir() {
        PathBuf::from("/tmp")
    } else {
        env::temp_dir()
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    base.join(format!("tgos-qperf-{}-{nonce}.sock", std::process::id()))
}

fn rooted_output_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub(super) fn ensure_file(path: &Path, label: &str) -> anyhow::Result<()> {
    if file_nonempty(path) {
        Ok(())
    } else {
        bail!("{label} not found or empty at {}", path.display())
    }
}

pub(super) fn file_nonempty(path: &Path) -> bool {
    path.metadata().map(|meta| meta.len() > 0).unwrap_or(false)
}

pub(super) fn count_lines(path: &Path) -> anyhow::Result<u64> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut count = 0;
    for line in BufReader::new(file).lines() {
        line?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::prepare_outputs;

    #[test]
    fn prepare_outputs_roots_relative_out_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outputs = prepare_outputs(
            root,
            "riscv64",
            "boot",
            Some(Path::new("target/qperf-test")),
            None,
        )
        .unwrap();

        assert!(outputs.work_dir.is_absolute());
        assert_eq!(outputs.work_dir, root.join("target"));
        assert_eq!(outputs.dir, root.join("target/qperf-test"));
        assert!(outputs.qmp_socket.is_absolute());
        assert!(outputs.qmp_socket.display().to_string().len() < 100);
    }

    #[test]
    fn prepare_outputs_roots_relative_output_dir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outputs = prepare_outputs(
            root,
            "riscv64",
            "boot",
            None,
            Some(Path::new("target/qperf-root")),
        )
        .unwrap();

        assert!(outputs.work_dir.is_absolute());
        assert_eq!(
            outputs.work_dir,
            root.join("target/qperf-root/perf/riscv64/latest")
        );
        assert_eq!(
            outputs.dir,
            root.join("target/qperf-root/perf/riscv64/latest/qperf")
        );
    }
}
