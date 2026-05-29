use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use object::{Object, ObjectSymbol};
use regex::Regex;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[clap(value_name = "INPUT")]
    input: Option<PathBuf>,
    #[clap(value_name = "OUTPUT")]
    output: Option<PathBuf>,
    #[clap(long, short, value_name = "ELF")]
    elf: Option<PathBuf>,
    #[clap(long, value_name = "N", default_value_t = 0)]
    top: usize,
    #[clap(long, value_name = "SVG")]
    flamegraph: Option<PathBuf>,
    #[clap(long, value_name = "SECONDS")]
    start_sec: Option<f64>,
    #[clap(long, value_name = "SECONDS")]
    stop_sec: Option<f64>,
    #[clap(long, value_name = "JSON")]
    stats: Option<PathBuf>,
    /// Folded-stack symbol style.
    #[clap(long, value_enum, default_value_t = SymbolStyle::Full)]
    symbol_style: SymbolStyle,
    /// Disable Rust symbol demangling.
    #[clap(long)]
    no_demangle: bool,
    /// Keep only samples whose resolved stack matches this regex.
    #[clap(long, value_name = "REGEX")]
    focus: Option<String>,
    /// Inferno flamegraph minimum frame width.
    #[clap(long, value_name = "PERCENT", default_value_t = 0.3)]
    min_percent: f64,
}

#[derive(Subcommand)]
enum Commands {
    /// Resolve symbols and produce folded stacks from qperf raw samples.
    Resolve(ResolveArgs),
    /// Compare two folded stack files and print top percentage deltas.
    Diff(DiffArgs),
}

#[derive(Args)]
struct ResolveArgs {
    #[clap(value_name = "INPUT")]
    input: PathBuf,
    #[clap(value_name = "OUTPUT")]
    output: PathBuf,
    #[clap(long, short, value_name = "ELF")]
    elf: PathBuf,
    /// Print top N hottest functions to stderr.
    #[clap(long, value_name = "N", default_value_t = 0)]
    top: usize,
    /// Generate flamegraph SVG when the analyzer was built with the flamegraph feature.
    #[clap(long, value_name = "SVG")]
    flamegraph: Option<PathBuf>,
    /// Keep samples at or after this elapsed time in seconds.
    #[clap(long, value_name = "SECONDS")]
    start_sec: Option<f64>,
    /// Keep samples before this elapsed time in seconds.
    #[clap(long, value_name = "SECONDS")]
    stop_sec: Option<f64>,
    /// Write resolve/filter statistics as JSON.
    #[clap(long, value_name = "JSON")]
    stats: Option<PathBuf>,
    /// Folded-stack symbol style.
    #[clap(long, value_enum, default_value_t = SymbolStyle::Full)]
    symbol_style: SymbolStyle,
    /// Disable Rust symbol demangling.
    #[clap(long)]
    no_demangle: bool,
    /// Keep only samples whose resolved stack matches this regex.
    #[clap(long, value_name = "REGEX")]
    focus: Option<String>,
    /// Inferno flamegraph minimum frame width.
    #[clap(long, value_name = "PERCENT", default_value_t = 0.3)]
    min_percent: f64,
}

#[derive(Args)]
struct DiffArgs {
    #[clap(long, value_name = "FILE")]
    baseline: PathBuf,
    #[clap(long, value_name = "FILE")]
    compare: PathBuf,
    #[clap(long, value_name = "SVG")]
    flamegraph: Option<PathBuf>,
    /// Print top N changed functions to stderr.
    #[clap(long, value_name = "N", default_value_t = 20)]
    top: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SymbolStyle {
    Full,
    Short,
    Module,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Resolve(args)) => cmd_resolve(args),
        Some(Commands::Diff(args)) => cmd_diff(args),
        None => {
            let input = cli.input.context("missing INPUT")?;
            let output = cli.output.context("missing OUTPUT")?;
            let elf = cli.elf.context("missing --elf")?;
            cmd_resolve(ResolveArgs {
                input,
                output,
                elf,
                top: cli.top,
                flamegraph: cli.flamegraph,
                start_sec: cli.start_sec,
                stop_sec: cli.stop_sec,
                stats: cli.stats,
                symbol_style: cli.symbol_style,
                no_demangle: cli.no_demangle,
                focus: cli.focus,
                min_percent: cli.min_percent,
            })
        }
    }
}

#[derive(bincode::Decode)]
struct SampleRecord {
    elapsed_ns: u64,
    trace: Vec<u64>,
}

struct DecodedRecord {
    elapsed_ns: Option<u64>,
    trace: Vec<u64>,
}

#[derive(Default)]
struct ResolveStats {
    format_version: u32,
    raw_records: u64,
    selected_records: u64,
    bad_records: u64,
    total_frames: u64,
    selected_frames: u64,
    samples_excluded_before: u64,
    samples_excluded_after: u64,
    first_sample_sec: Option<f64>,
    last_sample_sec: Option<f64>,
    start_sec: Option<f64>,
    stop_sec: Option<f64>,
    warning: Option<String>,
}

fn cmd_resolve(args: ResolveArgs) -> anyhow::Result<()> {
    if let (Some(start), Some(stop)) = (args.start_sec, args.stop_sec)
        && start >= stop
    {
        anyhow::bail!("--start-sec must be less than --stop-sec");
    }

    let elf_data = std::fs::read(&args.elf).context("failed to read ELF file")?;
    let elf_obj = object::File::parse(&*elf_data)
        .map_err(|err| anyhow::anyhow!("failed to parse ELF: {err}"))?;
    let loader = addr2line::Loader::new(&args.elf)
        .map_err(|err| anyhow::anyhow!("failed to create addr2line loader: {err}"))?;
    let focus = args
        .focus
        .as_deref()
        .map(Regex::new)
        .transpose()
        .context("invalid --focus regex")?;

    let mut output =
        BufWriter::new(File::create(&args.output).context("failed to create output file")?);
    let mut symbol_cache = HashMap::<u64, Vec<String>>::new();
    let mut hotspots: HashMap<String, u64> = HashMap::new();
    let mut stats = ResolveStats {
        start_sec: args.start_sec,
        stop_sec: args.stop_sec,
        ..ResolveStats::default()
    };
    let records = read_qperf_records(&args.input, &mut stats)?;

    for record in records {
        stats.raw_records += 1;
        stats.total_frames += record.trace.len() as u64;
        if let Some(elapsed_ns) = record.elapsed_ns {
            let elapsed_sec = elapsed_ns as f64 / 1_000_000_000.0;
            stats.first_sample_sec = Some(
                stats
                    .first_sample_sec
                    .map_or(elapsed_sec, |current| current.min(elapsed_sec)),
            );
            stats.last_sample_sec = Some(
                stats
                    .last_sample_sec
                    .map_or(elapsed_sec, |current| current.max(elapsed_sec)),
            );
            if args.start_sec.is_some_and(|start| elapsed_sec < start) {
                stats.samples_excluded_before += 1;
                continue;
            }
            if args.stop_sec.is_some_and(|stop| elapsed_sec >= stop) {
                stats.samples_excluded_after += 1;
                continue;
            }
        } else if args.start_sec.is_some() || args.stop_sec.is_some() {
            stats.warning =
                Some("input has no timestamps; elapsed-time filtering was not applied".to_string());
        }
        stats.selected_records += 1;
        let mut result = vec![];
        for (idx, ip) in record.trace.into_iter().enumerate() {
            if ip == 0 || ip == u64::MAX {
                continue;
            }
            let lookup_ip = if idx == 0 { ip } else { ip - 1 };
            let symbols = symbol_cache.entry(lookup_ip).or_insert_with(|| {
                resolve_symbols(
                    &loader,
                    &elf_obj,
                    lookup_ip,
                    !args.no_demangle,
                    args.symbol_style,
                )
            });
            result.extend(symbols.iter().cloned());
        }
        if result.is_empty() {
            stats.bad_records += 1;
            result.push("??".into());
        }
        if focus
            .as_ref()
            .is_some_and(|focus| !result.iter().any(|frame| focus.is_match(frame)))
        {
            continue;
        }
        stats.selected_frames += result.len() as u64;

        for func in &result {
            *hotspots.entry(func.clone()).or_insert(0) += 1;
        }

        result.reverse();
        writeln!(output, "{} 1", result.join(";")).context("failed to write folded output")?;
    }
    output.flush().context("failed to flush folded output")?;

    let total = hotspots.values().sum();
    print_hotspots(&hotspots, total, args.top);
    if let Some(stats_path) = args.stats {
        write_resolve_stats(&stats_path, &stats)?;
    }

    if let Some(svg_path) = args.flamegraph {
        generate_flamegraph(&args.output, &svg_path, args.min_percent)?;
    }

    Ok(())
}

fn read_qperf_records(path: &Path, stats: &mut ResolveStats) -> anyhow::Result<Vec<DecodedRecord>> {
    match read_qperf_records_v2(path) {
        Ok(records) => {
            stats.format_version = 2;
            Ok(records)
        }
        Err(v2_err) => match read_qperf_records_v1(path) {
            Ok(records) => {
                stats.format_version = 1;
                Ok(records)
            }
            Err(v1_err) => Err(v2_err).with_context(|| {
                format!(
                    "failed to decode qperf input as v2 records; v1 fallback also failed: {v1_err}"
                )
            }),
        },
    }
}

fn read_qperf_records_v2(path: &Path) -> anyhow::Result<Vec<DecodedRecord>> {
    let mut input = BufReader::new(File::open(path).context("failed to open input file")?);
    let mut records = Vec::new();
    loop {
        match bincode::decode_from_std_read(&mut input, bincode::config::standard()) {
            Ok(SampleRecord { elapsed_ns, trace }) => records.push(DecodedRecord {
                elapsed_ns: Some(elapsed_ns),
                trace,
            }),
            Err(err) => {
                if records.is_empty() {
                    return Err(err).context("failed to decode first qperf v2 record");
                }
                eprintln!(
                    "qperf-analyzer: stopped after {} v2 records: {err}",
                    records.len()
                );
                break;
            }
        }
    }
    Ok(records)
}

fn read_qperf_records_v1(path: &Path) -> anyhow::Result<Vec<DecodedRecord>> {
    let mut input = BufReader::new(File::open(path).context("failed to open input file")?);
    let mut records = Vec::new();
    loop {
        let trace: Vec<u64> =
            match bincode::decode_from_std_read(&mut input, bincode::config::standard()) {
                Ok(trace) => trace,
                Err(err) => {
                    if records.is_empty() {
                        return Err(err).context("failed to decode first qperf v1 record");
                    }
                    eprintln!(
                        "qperf-analyzer: stopped after {} v1 records: {err}",
                        records.len()
                    );
                    break;
                }
            };
        records.push(DecodedRecord {
            elapsed_ns: None,
            trace,
        });
    }
    Ok(records)
}

fn write_resolve_stats(path: &Path, stats: &ResolveStats) -> anyhow::Result<()> {
    let mut output = BufWriter::new(
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?,
    );
    writeln!(output, "{{")?;
    writeln!(output, "  \"format_version\": {},", stats.format_version)?;
    writeln!(output, "  \"raw_records\": {},", stats.raw_records)?;
    writeln!(
        output,
        "  \"selected_records\": {},",
        stats.selected_records
    )?;
    writeln!(output, "  \"bad_records\": {},", stats.bad_records)?;
    writeln!(output, "  \"total_frames\": {},", stats.total_frames)?;
    writeln!(output, "  \"selected_frames\": {},", stats.selected_frames)?;
    writeln!(
        output,
        "  \"samples_excluded_before\": {},",
        stats.samples_excluded_before
    )?;
    writeln!(
        output,
        "  \"samples_excluded_after\": {},",
        stats.samples_excluded_after
    )?;
    writeln!(
        output,
        "  \"first_sample_sec\": {},",
        json_optional_f64(stats.first_sample_sec)
    )?;
    writeln!(
        output,
        "  \"last_sample_sec\": {},",
        json_optional_f64(stats.last_sample_sec)
    )?;
    writeln!(
        output,
        "  \"start_sec\": {},",
        json_optional_f64(stats.start_sec)
    )?;
    writeln!(
        output,
        "  \"stop_sec\": {},",
        json_optional_f64(stats.stop_sec)
    )?;
    writeln!(
        output,
        "  \"warning\": {}",
        json_optional_string(stats.warning.as_deref())
    )?;
    writeln!(output, "}}")?;
    Ok(())
}

fn json_optional_f64(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map(|value| format!("{value:.9}"))
        .unwrap_or_else(|| "null".to_string())
}

fn json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!("{:?}", value))
        .unwrap_or_else(|| "null".to_string())
}

fn cmd_diff(args: DiffArgs) -> anyhow::Result<()> {
    let baseline = parse_folded(&args.baseline)?;
    let compare = parse_folded(&args.compare)?;
    let all_keys: BTreeSet<&String> = baseline.keys().chain(compare.keys()).collect();
    let baseline_total = baseline.values().sum::<u64>() as f64;
    let compare_total = compare.values().sum::<u64>() as f64;
    let mut diffs: Vec<(&String, f64, f64)> = Vec::new();

    let diff_folded = args
        .flamegraph
        .as_ref()
        .map(|svg| svg.with_extension("folded"));
    let mut diff_output = diff_folded
        .as_ref()
        .map(File::create)
        .transpose()
        .context("failed to create diff folded output")?
        .map(BufWriter::new);

    for key in &all_keys {
        let baseline_count = baseline.get(*key).copied().unwrap_or(0) as f64;
        let compare_count = compare.get(*key).copied().unwrap_or(0) as f64;
        let baseline_pct = percent(baseline_count, baseline_total);
        let compare_pct = percent(compare_count, compare_total);
        if baseline_count != compare_count {
            diffs.push((key, baseline_pct, compare_pct));
        }
        if let Some(output) = diff_output.as_mut() {
            writeln!(output, "{} {}", key, compare_count as u64)?;
        }
    }

    diffs.sort_by(|a, b| {
        let left = (a.2 - a.1).abs();
        let right = (b.2 - b.1).abs();
        right
            .partial_cmp(&left)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top = args.top.min(diffs.len());
    if top > 0 {
        eprintln!("\nTop {top} changed functions:");
        eprintln!(
            "{:<60} {:>10} {:>10} {:>10}",
            "Function", "Base%", "Comp%", "Delta%"
        );
        eprintln!("{}", "-".repeat(95));
        for (func, baseline_pct, compare_pct) in diffs.iter().take(top) {
            let delta = compare_pct - baseline_pct;
            eprintln!(
                "{:<60} {:>9.2}% {:>9.2}% {:>+9.2}%",
                truncate_str(func, 60),
                baseline_pct,
                compare_pct,
                delta
            );
        }
    }

    if let (Some(folded), Some(svg)) = (diff_folded, args.flamegraph) {
        if let Some(mut output) = diff_output {
            output.flush().ok();
        }
        generate_flamegraph(&folded, &svg, 0.3)?;
    }

    Ok(())
}

fn parse_folded(path: &Path) -> anyhow::Result<HashMap<String, u64>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut map = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(pos) = line.rfind(' ') else {
            continue;
        };
        let stack = &line[..pos];
        let count = line[pos + 1..].parse().unwrap_or(1);
        *map.entry(stack.to_string()).or_insert(0) += count;
    }
    Ok(map)
}

fn print_hotspots(hotspots: &HashMap<String, u64>, total: u64, top_n: usize) {
    if top_n == 0 || total == 0 {
        return;
    }
    let mut entries: Vec<_> = hotspots.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));

    let top = top_n.min(entries.len());
    eprintln!("\nTop {top} hottest functions (total samples: {total}):");
    eprintln!("{:<60} {:>10} {:>10}", "Function", "Samples", "Percent");
    eprintln!("{}", "-".repeat(82));
    for (func, count) in entries.iter().take(top) {
        let pct = **count as f64 / total as f64 * 100.0;
        eprintln!("{:<60} {:>10} {:>9.2}%", truncate_str(func, 60), count, pct);
    }
}

fn resolve_symbols(
    loader: &addr2line::Loader,
    elf_obj: &object::File<'_>,
    ip: u64,
    demangle: bool,
    style: SymbolStyle,
) -> Vec<String> {
    let mut result = Vec::new();
    let Ok(mut frames) = loader.find_frames(ip) else {
        return symtab_fallback(elf_obj, ip, demangle, style);
    };
    while let Ok(Some(frame)) = frames.next() {
        let func = if demangle {
            frame
                .function
                .as_ref()
                .and_then(|func| func.demangle().ok())
                .map(|name| name.into_owned())
        } else {
            frame
                .function
                .as_ref()
                .and_then(|func| func.raw_name().ok())
                .map(|name| name.into_owned())
        }
        .unwrap_or_else(|| "??".to_string());
        result.push(format_symbol_style(&func, style));
    }
    if result.is_empty() {
        symtab_fallback(elf_obj, ip, demangle, style)
    } else {
        result
    }
}

fn symtab_fallback(
    elf_obj: &object::File<'_>,
    ip: u64,
    demangle: bool,
    style: SymbolStyle,
) -> Vec<String> {
    let nearest = elf_obj
        .symbols()
        .filter_map(|symbol| {
            if !symbol.is_definition() || symbol.kind() != object::SymbolKind::Text {
                return None;
            }
            let addr = symbol.address();
            if ip < addr {
                return None;
            }
            let name = symbol.name().ok()?;
            if should_skip_symbol_name(name) {
                return None;
            }
            let offset = ip - addr;
            Some((offset, format_symbol_name(name, offset, demangle, style)))
        })
        .min_by_key(|(offset, _)| *offset);

    nearest
        .map(|(_, name)| vec![name])
        .unwrap_or_else(|| vec![format!("0x{ip:x}")])
}

fn should_skip_symbol_name(name: &str) -> bool {
    name.starts_with(".L") || name == "$x"
}

fn demangle_symbol(name: &str, demangle: bool) -> String {
    if demangle {
        rustc_demangle::try_demangle(name)
            .map(|name| name.to_string())
            .unwrap_or_else(|_| name.to_string())
    } else {
        name.to_string()
    }
}

fn format_symbol_style(name: &str, style: SymbolStyle) -> String {
    match style {
        SymbolStyle::Full => name.to_string(),
        SymbolStyle::Short => name.rsplit("::").next().unwrap_or(name).to_string(),
        SymbolStyle::Module => {
            let mut parts: Vec<_> = name.split("::").collect();
            if parts.len() <= 4 {
                return name.to_string();
            }
            let tail = parts.split_off(parts.len() - 3);
            format!("{}::{}", parts[0], tail.join("::"))
        }
    }
}

fn format_symbol_name(name: &str, offset: u64, demangle: bool, style: SymbolStyle) -> String {
    let name = format_symbol_style(&demangle_symbol(name, demangle), style);
    if offset == 0 {
        name
    } else {
        format!("{name}+0x{offset:x}")
    }
}

fn generate_flamegraph(
    folded_path: &Path,
    svg_path: &Path,
    min_percent: f64,
) -> anyhow::Result<()> {
    #[cfg(feature = "flamegraph")]
    {
        let input = File::open(folded_path)
            .with_context(|| format!("failed to open {}", folded_path.display()))?;
        let output = File::create(svg_path)
            .with_context(|| format!("failed to create {}", svg_path.display()))?;
        let mut opts = inferno::flamegraph::Options::default();
        opts.count_name = "samples".to_string();
        opts.title = "StarryOS qperf Flame Graph".to_string();
        opts.image_width = Some(3200);
        opts.frame_height = 24;
        opts.font_size = 13;
        opts.min_width = min_percent.max(0.0);
        opts.hash = true;
        opts.deterministic = true;
        inferno::flamegraph::from_reader(&mut opts, input, output)
            .context("failed to generate flamegraph")?;
        eprintln!("flamegraph written to {}", svg_path.display());
        Ok(())
    }
    #[cfg(not(feature = "flamegraph"))]
    {
        eprintln!(
            "flamegraph generation requires the qperf-analyzer flamegraph feature; use an \
             external generator, for example: inferno-flamegraph < {} > {}",
            folded_path.display(),
            svg_path.display()
        );
        let _ = min_percent;
        Ok(())
    }
}

fn percent(value: f64, total: f64) -> f64 {
    if total > 0.0 {
        value / total * 100.0
    } else {
        0.0
    }
}

fn truncate_str(value: &str, max_len: usize) -> &str {
    if value.len() <= max_len {
        value
    } else {
        &value[value.len() - max_len..]
    }
}
