use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;
use clap::{Parser, Subcommand};
use object::{Object, ObjectSymbol};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Resolve symbols and produce folded stacks from qperf raw samples
    Resolve(ResolveArgs),
    /// Compare two folded stack files and produce a diff
    Diff(DiffArgs),
}

#[derive(Parser)]
struct ResolveArgs {
    #[clap(value_name = "INPUT")]
    input: PathBuf,
    #[clap(value_name = "OUTPUT")]
    output: PathBuf,
    #[clap(long, short, value_name = "ELF")]
    elf: PathBuf,
    /// Print top N hottest functions to stderr
    #[clap(long, value_name = "N", default_value = "0")]
    top: usize,
    /// Generate flamegraph SVG (requires inferno crate feature)
    #[clap(long, value_name = "SVG")]
    flamegraph: Option<PathBuf>,
}

#[derive(Parser)]
struct DiffArgs {
    #[clap(long, value_name = "FILE")]
    baseline: PathBuf,
    #[clap(long, value_name = "FILE")]
    compare: PathBuf,
    #[clap(long, value_name = "SVG")]
    flamegraph: Option<PathBuf>,
    /// Print top N changed functions to stderr
    #[clap(long, value_name = "N", default_value = "20")]
    top: usize,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Resolve(args) => cmd_resolve(args),
        Commands::Diff(args) => cmd_diff(args),
    }
}

fn cmd_resolve(args: ResolveArgs) -> anyhow::Result<()> {
    let mut input = BufReader::new(File::open(&args.input).context("Failed to open input file")?);

    let elf_data = std::fs::read(&args.elf).context("Failed to read ELF file")?;
    let elf_obj = object::read::elf::ElfFile64::<object::Endianness>::parse(&*elf_data)
        .map_err(|e| anyhow::anyhow!("Failed to parse ELF: {e}"))?;

    let loader = addr2line::Loader::new(&args.elf)
        .map_err(|err| anyhow::anyhow!("Failed to create addr2line loader: {err}"))?;

    let mut output =
        BufWriter::new(File::create(&args.output).context("Failed to create output file")?);

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
                        return Err(err).context("Failed to decode first qperf record");
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
        for (i, ip) in trace.into_iter().enumerate() {
            if ip == 0 || ip == u64::MAX {
                continue;
            }
            let lookup_ip = if i == 0 { ip } else { ip - 1 };
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
        writeln!(output, "{} 1", result.join(";")).context("Failed to write to output file")?;
    }
    output.flush().context("Failed to flush output file")?;

    let total: u64 = hotspots.values().sum();
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

    let mut diffs: Vec<(&String, f64, f64)> = Vec::new();
    let baseline_total = baseline.values().sum::<u64>() as f64;
    let compare_total = compare.values().sum::<u64>() as f64;

    let diff_folded = std::env::temp_dir().join("qperf-diff-folded.txt");
    let mut diff_output = BufWriter::new(
        File::create(&diff_folded)
            .with_context(|| format!("Failed to create {}", diff_folded.display()))?,
    );
    let mut has_diff_content = false;

    for key in &all_keys {
        let b = baseline.get(*key).copied().unwrap_or(0) as f64;
        let c = compare.get(*key).copied().unwrap_or(0) as f64;
        let b_pct = if baseline_total > 0.0 {
            b / baseline_total * 100.0
        } else {
            0.0
        };
        let c_pct = if compare_total > 0.0 {
            c / compare_total * 100.0
        } else {
            0.0
        };
        if b != c {
            diffs.push((key, b_pct, c_pct));
        }
        if c > 0.0 {
            writeln!(diff_output, "{} {}", key, c as u64)?;
            has_diff_content = true;
        } else if b > 0.0 {
            writeln!(diff_output, "{} 0", key)?;
            has_diff_content = true;
        }
    }
    diff_output
        .flush()
        .context("Failed to flush diff folded file")?;

    diffs.sort_by(|a, b| {
        let da = (b.2 - b.1).abs();
        let db = (a.2 - a.1).abs();
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });

    let top = args.top.min(diffs.len());
    if top > 0 {
        eprintln!("\nTop {top} changed functions:");
        eprintln!(
            "{:<60} {:>10} {:>10} {:>10}",
            "Function", "Base%", "Comp%", "Delta%"
        );
        eprintln!("{}", "-".repeat(95));
        for (func, b_pct, c_pct) in diffs.iter().take(top) {
            let delta = c_pct - b_pct;
            eprintln!(
                "{:<60} {:>9.2}% {:>9.2}% {:>+9.2}%",
                truncate_str(func, 60),
                b_pct,
                c_pct,
                delta
            );
        }
    }

    if let Some(svg_path) = args.flamegraph
        && has_diff_content
    {
        generate_flamegraph(&diff_folded, &svg_path)?;
    }

    Ok(())
}

fn parse_folded(path: &Path) -> anyhow::Result<HashMap<String, u64>> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut map = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(pos) = line.rfind(' ') {
            let stack = &line[..pos];
            let count: u64 = line[pos + 1..].parse().unwrap_or(1);
            *map.entry(stack.to_string()).or_insert(0) += count;
        }
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
    elf_obj: &object::read::elf::ElfFile64<object::Endianness>,
    ip: u64,
) -> Vec<String> {
    let mut result = Vec::new();
    let Ok(mut frames) = loader.find_frames(ip) else {
        return symtab_fallback(elf_obj, ip);
    };
    while let Ok(Some(frame)) = frames.next() {
        let func = frame
            .function
            .as_ref()
            .and_then(|f| f.demangle().ok())
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

fn symtab_fallback(
    elf_obj: &object::read::elf::ElfFile64<object::Endianness>,
    ip: u64,
) -> Vec<String> {
    let best = elf_obj
        .symbols()
        .filter_map(|sym| {
            if !sym.is_definition() || sym.kind() != object::SymbolKind::Text {
                return None;
            }
            let addr = sym.address();
            let size = sym.size();
            if size == 0 {
                return None;
            }
            if ip >= addr && ip < addr + size {
                let name = sym.name().ok()?;
                let offset = ip - addr;
                Some((offset, format!("{name}+0x{offset:x}")))
            } else {
                None
            }
        })
        .min_by_key(|(offset, _)| *offset);

    match best {
        Some((_, name)) => vec![name],
        None => vec![format!("0x{ip:x}")],
    }
}

fn generate_flamegraph(folded_path: &Path, svg_path: &Path) -> anyhow::Result<()> {
    #[cfg(feature = "flamegraph")]
    {
        let input = File::open(folded_path)
            .with_context(|| format!("Failed to open {}", folded_path.display()))?;
        let output = File::create(svg_path)
            .with_context(|| format!("Failed to create {}", svg_path.display()))?;
        let mut opts = inferno::flamegraph::Options::default();
        opts.count_name = "samples".to_string();
        inferno::flamegraph::from_reader(&mut opts, input, output)
            .with_context(|| "Failed to generate flamegraph")?;
        eprintln!("Flamegraph written to {}", svg_path.display());
        Ok(())
    }
    #[cfg(not(feature = "flamegraph"))]
    {
        eprintln!(
            "Flamegraph generation requires the 'flamegraph' feature. Rebuild with: cargo build \
             --features flamegraph\nAlternatively, use an external tool: inferno-flamegraph < {} \
             > {}",
            folded_path.display(),
            svg_path.display()
        );
        Ok(())
    }
}

fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let start = s.len() - max_len + 3;
        &s[start - 3..]
    }
}
