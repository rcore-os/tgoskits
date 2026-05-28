use std::{fs, path::PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::{Args as ClapArgs, Subcommand};
use object::{
    Object, ObjectSymbol,
    read::elf::{FileHeader, ProgramHeader},
};

use crate::{
    axvisor::{ArgsBuild, Axvisor},
    backtrace,
    context::SnapshotPersistence,
};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Check whether the built Axvisor ELF matches the bare-bin HTTP Boot shape.
    Check(ArgsCheck),
}

#[derive(ClapArgs)]
pub struct ArgsCheck {
    #[command(flatten)]
    pub build: ArgsBuild,

    /// Inspect an existing ELF instead of building first.
    #[arg(long, value_name = "PATH")]
    pub elf: Option<PathBuf>,
}

pub(super) async fn run(axvisor: &mut Axvisor, args: Args) -> anyhow::Result<()> {
    match args.command {
        Command::Check(args) => check(axvisor, args).await,
    }
}

async fn check(axvisor: &mut Axvisor, args: ArgsCheck) -> anyhow::Result<()> {
    let request = axvisor.prepare_request(
        (&args.build).into(),
        None,
        None,
        SnapshotPersistence::Discard,
    )?;

    if request.arch != "x86_64" {
        bail!(
            "Axvisor HTTP Boot phase-0 check currently targets x86_64, got {}",
            request.arch
        );
    }

    axvisor.app.set_debug_mode(request.debug)?;

    let elf_path = match args.elf {
        Some(path) => path,
        None => {
            let cargo = super::build::load_cargo_config(&request)?;
            axvisor
                .app
                .build(cargo, request.build_info_path.clone())
                .await?;
            backtrace::arceos_rust_elf_path(
                axvisor.app.workspace_root(),
                &request.target,
                &request.package,
                request.debug,
            )
        }
    };

    let report = inspect_elf(&elf_path)?;
    report.print(&elf_path);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentInfo {
    index: usize,
    vaddr: u64,
    paddr: u64,
    file_size: u64,
    mem_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpbootElfReport {
    entry: u64,
    load_addr: u64,
    image_file_size: u64,
    image_mem_size: u64,
    entry_offset: u64,
    entry_paddr: u64,
    start_symbol: Option<u64>,
    main_symbol: Option<u64>,
    load_segments: Vec<SegmentInfo>,
}

impl HttpbootElfReport {
    fn print(&self, elf_path: &PathBuf) {
        println!("=== Axvisor x86_64 HTTP Boot phase-0 check ===");
        println!("elf: {}", elf_path.display());
        println!("kernel_load_addr: {}", hex(self.load_addr));
        println!("elf_entry: {}", hex(self.entry));
        println!("entry_offset_from_load: {}", hex(self.entry_offset));
        println!("entry_paddr_for_manifest: {}", hex(self.entry_paddr));
        println!("flat_binary_file_size: {}", hex(self.image_file_size));
        println!("flat_binary_memory_span: {}", hex(self.image_mem_size));

        for segment in &self.load_segments {
            println!(
                "load_segment[{}]: paddr={} vaddr={} filesz={} memsz={}",
                segment.index,
                hex(segment.paddr),
                hex(segment.vaddr),
                hex(segment.file_size),
                hex(segment.mem_size)
            );
        }

        println!("_start: {}", option_hex(self.start_symbol));
        println!("main: {}", option_hex(self.main_symbol));
        println!("phase0_status: partial");
        println!(
            "phase0_note: image has a compact physical load span, but the current x86_64 entry is \
             the Multiboot _start path. A LoongArch-style UEFI loader still needs either a direct \
             HTTP Boot entry wrapper or Multiboot context synthesis before jump."
        );
        println!(
            "manifest_v1_candidate: kernel_load_addr={} entry_point={}",
            hex(self.load_addr),
            hex(self.entry_paddr)
        );
    }
}

fn inspect_elf(path: &PathBuf) -> anyhow::Result<HttpbootElfReport> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let elf = object::read::elf::ElfFile64::parse(bytes.as_slice())
        .map_err(|e| anyhow!("failed to parse x86_64 ELF {}: {e}", path.display()))?;
    let endian = elf.endian();
    let entry: u64 = elf.elf_header().e_entry(endian).into();

    let mut load_segments = Vec::new();
    for (index, header) in elf.elf_program_headers().iter().enumerate() {
        if header.p_type(endian) != object::elf::PT_LOAD {
            continue;
        }
        let file_size = header.p_filesz(endian).into();
        let mem_size = header.p_memsz(endian).into();
        if mem_size == 0 {
            continue;
        }
        load_segments.push(SegmentInfo {
            index,
            vaddr: header.p_vaddr(endian).into(),
            paddr: header.p_paddr(endian).into(),
            file_size,
            mem_size,
        });
    }

    if load_segments.is_empty() {
        bail!("{} has no loadable ELF segments", path.display());
    }

    load_segments.sort_by_key(|segment| segment.paddr);

    let load_addr = load_segments[0].paddr;
    let image_file_end = max_segment_end(&load_segments, |segment| segment.file_size)?;
    let image_mem_end = max_segment_end(&load_segments, |segment| segment.mem_size)?;
    let first = &load_segments[0];
    let entry_offset = entry
        .checked_sub(first.vaddr)
        .ok_or_else(|| anyhow!("ELF entry is below first load segment virtual address"))?;
    let entry_paddr = first
        .paddr
        .checked_add(entry_offset)
        .ok_or_else(|| anyhow!("entry physical address overflows u64"))?;

    Ok(HttpbootElfReport {
        entry,
        load_addr,
        image_file_size: image_file_end - load_addr,
        image_mem_size: image_mem_end - load_addr,
        entry_offset,
        entry_paddr,
        start_symbol: find_symbol(&elf, "_start"),
        main_symbol: find_symbol(&elf, "main"),
        load_segments,
    })
}

fn max_segment_end(
    segments: &[SegmentInfo],
    size: impl Fn(&SegmentInfo) -> u64,
) -> anyhow::Result<u64> {
    segments
        .iter()
        .map(|segment| {
            segment
                .paddr
                .checked_add(size(segment))
                .ok_or_else(|| anyhow!("load segment end address overflows u64"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("load segment list should not be empty")
}

fn find_symbol(elf: &object::read::elf::ElfFile64<'_>, name: &str) -> Option<u64> {
    elf.symbols()
        .find(|symbol| symbol.name().ok() == Some(name))
        .map(|symbol| symbol.address())
}

fn hex(value: u64) -> String {
    format!("{value:#x}")
}

fn option_hex(value: Option<u64>) -> String {
    value.map(hex).unwrap_or_else(|| "missing".to_string())
}
