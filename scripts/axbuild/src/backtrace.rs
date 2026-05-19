use std::{fs, io::Read, path::PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use regex::Regex;

#[derive(Subcommand)]
pub enum Command {
    /// Extract and symbolize BACKTRACE_BEGIN/BT/BACKTRACE_END blocks from text logs.
    Symbolize(SymbolizeArgs),
}

#[derive(Args)]
pub struct SymbolizeArgs {
    /// Path to the kernel/app ELF file to symbolize addresses against.
    #[arg(long, value_name = "PATH")]
    pub elf: PathBuf,

    /// Path to the captured log. If omitted, read from stdin.
    #[arg(long, value_name = "PATH")]
    pub log: Option<PathBuf>,

    /// Only symbolize blocks whose kind matches this value.
    #[arg(long, value_name = "KIND")]
    pub kind: Option<String>,

    /// Subtract 1 from ip before symbolization (matches typical call-site adjustment).
    ///
    /// Use `--adjust-ip false` to disable.
    #[arg(
        long,
        value_name = "BOOL",
        default_value_t = true,
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = clap::value_parser!(bool)
    )]
    pub adjust_ip: bool,

    /// Apply a signed bias to ip before symbolization (useful when runtime addresses are slid).
    ///
    /// Example: `--ip-bias -0xffff_ffff_8000_0000`.
    #[arg(long, value_name = "I64", default_value_t = 0)]
    pub ip_bias: i64,
}

pub fn execute(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Symbolize(args) => symbolize(args),
    }
}

#[derive(Debug, Clone)]
struct Frame {
    idx: usize,
    ip: u64,
    fp: Option<u64>,
}

#[derive(Debug, Clone)]
struct Block {
    kind: String,
    arch: Option<String>,
    frames: Vec<Frame>,
    errors: Vec<String>,
}

fn read_text(log: Option<PathBuf>) -> anyhow::Result<String> {
    match log {
        Some(path) => Ok(fs::read_to_string(&path)
            .with_context(|| format!("failed to read log {}", path.display()))?),
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("failed to read stdin")?;
            Ok(s)
        }
    }
}

fn parse_blocks(text: &str) -> anyhow::Result<Vec<Block>> {
    let begin_re = Regex::new(r"BACKTRACE_BEGIN\b.*\bkind=([^\s]+)\b(?:.*\barch=([^\s]+)\b)?")
        .context("invalid begin regex")?;
    let frame_re = Regex::new(r"\bBT\s+(\d+)\s+ip=0x([0-9a-fA-F]+)(?:\s+fp=0x([0-9a-fA-F]+))?")
        .context("invalid frame regex")?;
    let error_re = Regex::new(r"\bBT_ERROR\s+([^\s]+)").context("invalid error regex")?;
    let end_re = Regex::new(r"BACKTRACE_END\b").context("invalid end regex")?;

    #[derive(Debug)]
    enum State {
        Idle,
        Capturing(Block),
    }

    let mut state = State::Idle;
    let mut out = Vec::new();

    for line in text.lines() {
        match &mut state {
            State::Idle => {
                if let Some(cap) = begin_re.captures(line) {
                    let kind = cap.get(1).unwrap().as_str().to_string();
                    let arch = cap.get(2).map(|m| m.as_str().to_string());
                    state = State::Capturing(Block {
                        kind,
                        arch,
                        frames: Vec::new(),
                        errors: Vec::new(),
                    });
                }
            }
            State::Capturing(block) => {
                if let Some(cap) = begin_re.captures(line) {
                    out.push(block.clone());
                    let kind = cap.get(1).unwrap().as_str().to_string();
                    let arch = cap.get(2).map(|m| m.as_str().to_string());
                    *block = Block {
                        kind,
                        arch,
                        frames: Vec::new(),
                        errors: Vec::new(),
                    };
                    continue;
                }
                if end_re.is_match(line) {
                    out.push(block.clone());
                    state = State::Idle;
                    continue;
                }

                if let Some(cap) = frame_re.captures(line) {
                    let idx: usize = cap.get(1).unwrap().as_str().parse()?;
                    let ip = u64::from_str_radix(cap.get(2).unwrap().as_str(), 16)?;
                    let fp = cap
                        .get(3)
                        .map(|m| u64::from_str_radix(m.as_str(), 16))
                        .transpose()?;
                    block.frames.push(Frame { idx, ip, fp });
                    continue;
                }

                if let Some(cap) = error_re.captures(line) {
                    let err = cap.get(1).unwrap().as_str().to_string();
                    block.errors.push(err);
                }
            }
        }
    }

    if let State::Capturing(block) = state {
        out.push(block);
    }

    Ok(out)
}

fn symbolize(args: SymbolizeArgs) -> anyhow::Result<()> {
    let text = read_text(args.log)?;
    let blocks = parse_blocks(&text)?;
    if blocks.is_empty() {
        bail!("no backtrace blocks found");
    }

    let loader = addr2line::Loader::new(&args.elf).map_err(|err| {
        anyhow::anyhow!(
            "failed to load dwarf/symbols from {}: {}",
            args.elf.display(),
            err
        )
    })?;

    for (i, block) in blocks.iter().enumerate() {
        if let Some(kind) = &args.kind
            && &block.kind != kind
        {
            continue;
        }

        println!(
            "BACKTRACE_BLOCK {} kind={} arch={}",
            i,
            block.kind,
            block.arch.as_deref().unwrap_or("?")
        );

        for frame in &block.frames {
            let ip = if args.adjust_ip && frame.ip > 0 {
                frame.ip - 1
            } else {
                frame.ip
            };
            let ip = ip.wrapping_add_signed(args.ip_bias);
            let symbolized = maybe_symbolize_with_loader(&loader, ip);

            match (&frame.fp, symbolized) {
                (Some(fp), Some(sym)) => {
                    println!("BT {} ip=0x{:x} fp=0x{:x} {}", frame.idx, frame.ip, fp, sym);
                }
                (Some(fp), None) => {
                    println!("BT {} ip=0x{:x} fp=0x{:x}", frame.idx, frame.ip, fp);
                }
                (None, Some(sym)) => {
                    println!("BT {} ip=0x{:x} {}", frame.idx, frame.ip, sym);
                }
                (None, None) => {
                    println!("BT {} ip=0x{:x}", frame.idx, frame.ip);
                }
            };
        }

        for err in &block.errors {
            println!("BT_ERROR {}", err);
        }
    }

    Ok(())
}

fn maybe_symbolize_with_loader(loader: &addr2line::Loader, ip: u64) -> Option<String> {
    if ip == 0 {
        return None;
    }
    symbolize_with_loader(loader, ip)
}

fn symbolize_with_loader(loader: &addr2line::Loader, ip: u64) -> Option<String> {
    let mut frames = loader.find_frames(ip).ok()?;
    let mut out = Vec::new();
    while let Some(frame) = frames.next().ok()? {
        let name = frame
            .function
            .as_ref()
            .and_then(|f| f.raw_name().ok())
            .map(|s| rustc_demangle::demangle(s.as_ref()).to_string());
        let loc = frame.location.as_ref().and_then(|l| {
            let file = l.file?;
            let line = l.line?;
            Some(format!("{file}:{line}"))
        });
        match (name, loc) {
            (Some(name), Some(loc)) => out.push(format!("{name} ({loc})")),
            (Some(name), None) => out.push(name),
            (None, Some(loc)) => out.push(loc),
            (None, None) => {}
        }
    }
    if out.is_empty() {
        let sym = loader
            .find_symbol(ip)
            .map(|s| rustc_demangle::demangle(s).to_string());
        return sym;
    }

    Some(out.join(" ; "))
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use object::{Object, ObjectSymbol};

    use super::*;

    #[unsafe(no_mangle)]
    extern "C" fn bt_symbolize_probe() {
        std::hint::black_box(());
    }

    #[test]
    fn parse_blocks_extracts_frames_with_prefix_noise() {
        let text = r#"
[0.000] INFO something
[0.001] BACKTRACE_BEGIN kind=panic arch=x86_64 alloc=true dwarf=false
[0.001] BT 0 ip=0x1000 fp=0x2000
[0.001] BT 1 ip=0x1010 fp=0x2010
[0.002] BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[0].arch.as_deref(), Some("x86_64"));
        assert_eq!(blocks[0].frames.len(), 2);
        assert_eq!(blocks[0].frames[0].idx, 0);
        assert_eq!(blocks[0].frames[0].ip, 0x1000);
        assert_eq!(blocks[0].frames[0].fp, Some(0x2000));
    }

    #[test]
    fn parse_blocks_accepts_missing_end_marker() {
        let text = r#"
BACKTRACE_BEGIN kind=trap arch=riscv64
BT 0 ip=0xdead fp=0xbeef
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "trap");
        assert_eq!(blocks[0].frames.len(), 1);
    }

    #[test]
    fn parse_blocks_splits_blocks_when_begin_repeats() {
        let text = r#"
BACKTRACE_BEGIN kind=panic arch=x86_64
BT 0 ip=0x1000 fp=0x2000
BACKTRACE_BEGIN kind=trap arch=x86_64
BT 0 ip=0x3000 fp=0x4000
BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[1].kind, "trap");
    }

    #[test]
    fn parse_blocks_captures_bt_error() {
        let text = r#"
BACKTRACE_BEGIN kind=panic arch=aarch64 alloc=false dwarf=false
BT_ERROR requires_alloc
BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, "panic");
        assert_eq!(blocks[0].errors, vec!["requires_alloc".to_string()]);
        assert!(blocks[0].frames.is_empty());
    }

    #[test]
    fn parse_blocks_accepts_missing_fp() {
        let text = r#"
BACKTRACE_BEGIN kind=trap arch=riscv64
BT 0 ip=0xdead
BACKTRACE_END
"#;
        let blocks = parse_blocks(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].frames.len(), 1);
        assert_eq!(blocks[0].frames[0].ip, 0xdead);
        assert_eq!(blocks[0].frames[0].fp, None);
    }

    #[test]
    fn cli_accepts_adjust_ip_false() {
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = TestCli::try_parse_from([
            "tg-xtask",
            "symbolize",
            "--elf",
            "/tmp/fake.elf",
            "--adjust-ip",
            "false",
        ])
        .unwrap();

        let Command::Symbolize(args) = cli.command;
        assert!(!args.adjust_ip);
    }

    #[test]
    fn symbolize_resolves_symbol_with_ip_bias_under_aslr() {
        let exe = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&exe).unwrap();
        let obj = object::File::parse(bytes.as_slice()).unwrap();

        let runtime_ip = bt_symbolize_probe as *const () as usize as u64;
        let mut file_ip = None;
        for sym in obj.symbols() {
            let Ok(name) = sym.name() else {
                continue;
            };
            if name == "bt_symbolize_probe" || name == "_bt_symbolize_probe" {
                file_ip = Some(sym.address());
                break;
            }
        }
        let file_ip = file_ip.expect("failed to find bt_symbolize_probe symbol in current exe");

        let bias = file_ip as i64 - runtime_ip as i64;
        let ip_for_file = runtime_ip.wrapping_add_signed(bias);

        let loader = addr2line::Loader::new(&exe).unwrap();
        let sym = symbolize_with_loader(&loader, ip_for_file).unwrap();
        assert!(sym.contains("bt_symbolize_probe"));
    }

    #[test]
    fn symbolize_skips_zero_ip() {
        let exe = std::env::current_exe().unwrap();
        let loader = addr2line::Loader::new(&exe).unwrap();
        assert!(maybe_symbolize_with_loader(&loader, 0).is_none());
    }
}
