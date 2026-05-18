use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::PathBuf,
};

use anyhow::Context;
use clap::Parser;

#[derive(Parser)]
struct Cli {
    #[clap(value_name = "INPUT")]
    input: PathBuf,
    #[clap(value_name = "OUTPUT")]
    output: PathBuf,
    #[clap(long, short, value_name = "ELF")]
    elf: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut input = BufReader::new(File::open(&cli.input).context("Failed to open input file")?);

    let loader = addr2line::Loader::new(&cli.elf)
        .map_err(|err| anyhow::anyhow!("Failed to create addr2line loader: {err}"))?;

    let mut output =
        BufWriter::new(File::create(&cli.output).context("Failed to create output file")?);
    let mut symbol_cache = HashMap::<u64, Vec<String>>::new();
    let mut decoded = 0u64;
    let mut bad_records = 0u64;

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
                .or_insert_with(|| resolve_symbols(&loader, lookup_ip));
            result.extend(symbols.iter().cloned());
        }
        if result.is_empty() {
            bad_records += 1;
            result.push("??".into());
        }

        result.reverse();
        writeln!(output, "{} 1", result.join(";")).context("Failed to write to output file")?;
    }
    output.flush().context("Failed to flush output file")?;
    Ok(())
}

fn resolve_symbols(loader: &addr2line::Loader, ip: u64) -> Vec<String> {
    let mut result = Vec::new();
    let Ok(mut frames) = loader.find_frames(ip) else {
        return vec![format!("0x{ip:x}")];
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
        result.push(format!("0x{ip:x}"));
    }
    result
}
