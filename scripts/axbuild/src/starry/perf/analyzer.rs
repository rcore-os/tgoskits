use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail};

use super::{
    super::{ArgsPerf, PerfFlamegraphKind},
    args_support::flamegraph_min_percent,
    harness::QperfTools,
    monitor::PerfWindowReport,
    outputs::{PerfOutputs, ensure_file, file_nonempty},
    toolchain::find_executable,
};
use crate::support::process::ProcessExt;

pub(super) struct AnalyzerRun<'a> {
    pub(super) analyzer: &'a Path,
    pub(super) elf: &'a Path,
    pub(super) raw: &'a Path,
    pub(super) folded: &'a Path,
    pub(super) flamegraph: &'a Path,
    pub(super) resolve_stats: &'a Path,
    pub(super) depth_summary: Option<&'a Path>,
    pub(super) generate_svg: bool,
    pub(super) top_n: usize,
    pub(super) start_sec: Option<f64>,
    pub(super) stop_sec: Option<f64>,
    pub(super) symbol_style: String,
    pub(super) demangle: bool,
    pub(super) focus: Option<&'a str>,
    pub(super) min_percent: f64,
}

pub(super) fn run_analyzer(args: AnalyzerRun<'_>) -> anyhow::Result<()> {
    ensure_file(args.elf, "StarryOS kernel ELF")?;
    ensure_file(args.raw, "qperf raw samples")?;
    let mut command = Command::new(args.analyzer);
    command
        .arg("resolve")
        .arg("-e")
        .arg(args.elf)
        .arg(args.raw)
        .arg(args.folded);
    if args.top_n > 0 {
        command.arg("--top").arg(args.top_n.to_string());
    }
    if let Some(start_sec) = args.start_sec {
        command.arg("--start-sec").arg(format!("{start_sec:.9}"));
    }
    if let Some(stop_sec) = args.stop_sec {
        command.arg("--stop-sec").arg(format!("{stop_sec:.9}"));
    }
    command
        .arg("--symbol-style")
        .arg(&args.symbol_style)
        .arg("--min-percent")
        .arg(args.min_percent.to_string());
    if !args.demangle {
        command.arg("--no-demangle");
    }
    if let Some(focus) = args.focus {
        command.arg("--focus").arg(focus);
    }
    command.arg("--stats").arg(args.resolve_stats);
    if let Some(depth_summary) = args.depth_summary {
        command.arg("--depth-summary").arg(depth_summary);
    }
    if args.generate_svg {
        command.arg("--flamegraph").arg(args.flamegraph);
    }
    command.exec().context("failed to run qperf-analyzer")?;
    if !args.folded.exists() {
        bail!("folded stack output not found at {}", args.folded.display());
    }
    Ok(())
}

pub(super) fn generate_phase_flamegraphs(
    tools: &QperfTools,
    elf: &Path,
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    window: &PerfWindowReport,
    generate_svg: bool,
) -> anyhow::Result<()> {
    if window.start_time.is_some() && window.stop_time.is_some() {
        fs::copy(&outputs.folded, &outputs.folded_workload).with_context(|| {
            format!(
                "failed to copy workload folded stack to {}",
                outputs.folded_workload.display()
            )
        })?;
        if file_nonempty(&outputs.flamegraph) {
            fs::copy(&outputs.flamegraph, &outputs.flamegraph_workload).with_context(|| {
                format!(
                    "failed to copy workload flamegraph to {}",
                    outputs.flamegraph_workload.display()
                )
            })?;
        }
    }
    if let Some(start_sec) = window.start_time {
        run_analyzer(AnalyzerRun {
            analyzer: &tools.analyzer,
            elf,
            raw: &outputs.raw,
            folded: &outputs.folded_boot,
            flamegraph: &outputs.flamegraph_boot,
            resolve_stats: &outputs
                .resolve_stats
                .with_file_name("resolve.boot.stats.json"),
            depth_summary: Some(
                &outputs
                    .stack_depth_summary
                    .with_file_name("stack-depth-summary.boot.csv"),
            ),
            generate_svg,
            top_n: 0,
            start_sec: None,
            stop_sec: Some(start_sec),
            symbol_style: args.symbol_style.to_string(),
            demangle: true,
            focus: None,
            min_percent: flamegraph_min_percent(args),
        })?;
    }
    if let Some(stop_sec) = window.stop_time {
        run_analyzer(AnalyzerRun {
            analyzer: &tools.analyzer,
            elf,
            raw: &outputs.raw,
            folded: &outputs.folded_post,
            flamegraph: &outputs.flamegraph_post,
            resolve_stats: &outputs
                .resolve_stats
                .with_file_name("resolve.post.stats.json"),
            depth_summary: Some(
                &outputs
                    .stack_depth_summary
                    .with_file_name("stack-depth-summary.post.csv"),
            ),
            generate_svg,
            top_n: 0,
            start_sec: Some(stop_sec),
            stop_sec: None,
            symbol_style: args.symbol_style.to_string(),
            demangle: true,
            focus: None,
            min_percent: flamegraph_min_percent(args),
        })?;
    }
    Ok(())
}

pub(super) fn generate_focus_flamegraph(
    tools: &QperfTools,
    elf: &Path,
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    generate_svg: bool,
) -> anyhow::Result<()> {
    let Some(focus) = args.focus.as_deref() else {
        return Ok(());
    };
    run_analyzer(AnalyzerRun {
        analyzer: &tools.analyzer,
        elf,
        raw: &outputs.raw,
        folded: &outputs.folded_focus,
        flamegraph: &outputs.flamegraph_focus,
        resolve_stats: &outputs
            .resolve_stats
            .with_file_name("resolve.focus.stats.json"),
        depth_summary: Some(
            &outputs
                .stack_depth_summary
                .with_file_name("stack-depth-summary.focus.csv"),
        ),
        generate_svg,
        top_n: 0,
        start_sec: None,
        stop_sec: None,
        symbol_style: args.symbol_style.to_string(),
        demangle: true,
        focus: Some(focus),
        min_percent: flamegraph_min_percent(args),
    })
}

pub(super) fn write_flamegraph_html(
    outputs: &PerfOutputs,
    kind: PerfFlamegraphKind,
    flamegraph_generated: bool,
) -> anyhow::Result<()> {
    if !matches!(kind, PerfFlamegraphKind::Html) || !flamegraph_generated {
        return Ok(());
    }
    let svg = outputs
        .flamegraph
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("flamegraph.svg");
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>StarryOS qperf Flame \
         Graph</title><style>body{{margin:0}}object{{width:100vw;height:100vh;border:0}}</\
         style><object data=\"{svg}\" type=\"image/svg+xml\"></object>\n"
    );
    fs::write(&outputs.flamegraph_html, html)
        .with_context(|| format!("failed to write {}", outputs.flamegraph_html.display()))
}

pub(super) fn try_generate_flamegraph(folded: &Path, svg: &Path) -> anyhow::Result<bool> {
    let Some(generator) = find_flamegraph_generator() else {
        eprintln!(
            "qperf: flamegraph generator not found; install inferno-flamegraph or flamegraph.pl \
             to generate {}",
            svg.display()
        );
        return Ok(false);
    };

    let input =
        File::open(folded).with_context(|| format!("failed to open {}", folded.display()))?;
    let output =
        File::create(svg).with_context(|| format!("failed to create {}", svg.display()))?;
    let status = Command::new(&generator)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .status()
        .with_context(|| format!("failed to run {}", generator.display()))?;
    if !status.success() {
        eprintln!(
            "qperf: flamegraph generator {} failed with {status}; folded stacks are still at {}",
            generator.display(),
            folded.display()
        );
        return Ok(false);
    }
    Ok(true)
}

fn find_flamegraph_generator() -> Option<PathBuf> {
    ["inferno-flamegraph", "flamegraph", "flamegraph.pl"]
        .into_iter()
        .find_map(find_executable)
}
