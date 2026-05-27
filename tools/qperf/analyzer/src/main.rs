use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use object::{Object, ObjectSymbol};

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
            })
        }
    }
}

fn cmd_resolve(args: ResolveArgs) -> anyhow::Result<()> {
    let mut input = BufReader::new(File::open(&args.input).context("failed to open input file")?);
    let elf_data = std::fs::read(&args.elf).context("failed to read ELF file")?;
    let elf_obj = object::File::parse(&*elf_data)
        .map_err(|err| anyhow::anyhow!("failed to parse ELF: {err}"))?;
    let loader = addr2line::Loader::new(&args.elf)
        .map_err(|err| anyhow::anyhow!("failed to create addr2line loader: {err}"))?;

    let mut output =
        BufWriter::new(File::create(&args.output).context("failed to create output file")?);
    let mut symbol_cache = HashMap::<u64, Vec<String>>::new();
    let mut decoded = 0u64;
    let mut bad_records = 0u64;
    let mut hotspots: HashMap<String, u64> = HashMap::new();

    loop {
        let trace: Vec<u64> =
            match bincode::decode_from_std_read(&mut input, bincode::config::standard()) {
                Ok(trace) => trace,
                Err(err) => {
                    if decoded == 0 {
                        return Err(err).context("failed to decode first qperf record");
                    }
                    eprintln!(
                        "qperf-analyzer: stopped after {decoded} records ({bad_records} bad \
                         records): {err}"
                    );
                    break;
                }
            };
        decoded += 1;
        let mut result = vec![];
        for (idx, ip) in trace.into_iter().enumerate() {
            if ip == 0 || ip == u64::MAX {
                continue;
            }
            let lookup_ip = if idx == 0 { ip } else { ip - 1 };
            let symbols = symbol_cache
                .entry(lookup_ip)
                .or_insert_with(|| resolve_symbols(&loader, &elf_obj, lookup_ip));
            result.extend(symbols.iter().cloned());
        }
        if result.is_empty() {
            bad_records += 1;
            result.push("??".into());
        }

        for func in &result {
            *hotspots.entry(func.clone()).or_insert(0) += 1;
        }

        result.reverse();
        writeln!(output, "{} 1", result.join(";")).context("failed to write folded output")?;
    }
    output.flush().context("failed to flush folded output")?;

    let total = hotspots.values().sum();
    print_hotspots(&hotspots, total, args.top);

    if let Some(svg_path) = args.flamegraph {
        generate_flamegraph(&args.output, &svg_path)?;
    }

    Ok(())
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
        generate_flamegraph(&folded, &svg)?;
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

fn resolve_symbols(loader: &addr2line::Loader, elf_obj: &object::File<'_>, ip: u64) -> Vec<String> {
    let mut result = Vec::new();
    let Ok(mut frames) = loader.find_frames(ip) else {
        return symtab_fallback(elf_obj, ip);
    };
    while let Ok(Some(frame)) = frames.next() {
        let func = frame
            .function
            .as_ref()
            .and_then(|func| func.demangle().ok())
            .unwrap_or("??".into())
            .into_owned();
        result.push(func);
    }
    if result.is_empty() {
        symtab_fallback(elf_obj, ip)
    } else {
        result
    }
}

fn symtab_fallback(elf_obj: &object::File<'_>, ip: u64) -> Vec<String> {
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
            Some((offset, format_symbol_name(name, offset)))
        })
        .min_by_key(|(offset, _)| *offset);

    nearest
        .map(|(_, name)| vec![name])
        .unwrap_or_else(|| vec![format!("0x{ip:x}")])
}

fn should_skip_symbol_name(name: &str) -> bool {
    name.starts_with(".L") || name == "$x"
}

fn format_symbol_name(name: &str, offset: u64) -> String {
    if offset == 0 {
        name.to_string()
    } else {
        format!("{name}+0x{offset:x}")
    }
}

fn generate_flamegraph(folded_path: &Path, svg_path: &Path) -> anyhow::Result<()> {
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
        opts.min_width = 0.35;
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
