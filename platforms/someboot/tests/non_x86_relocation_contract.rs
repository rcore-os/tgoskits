#![cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little"
))]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[path = "support/elf_image.rs"]
pub mod elf_image;
// Compile the exact production RELA walker into this host-side test so the
// load-bias fixture cannot drift into a separate relocation algorithm.
#[path = "../src/elf.rs"]
pub mod production_elf;

use elf_image::{ElfImage, MappingPermissions};

const PERCPU_MAGIC: usize = 0x5a5a_a5a5_1357_2468;
const RUNTIME_LOAD_COUNT: usize = 3;

#[derive(Clone, Copy)]
struct Architecture {
    name: &'static str,
    clang_target: &'static str,
    elf_machine: &'static str,
    production_relocation_source: &'static str,
    relative_relocation_name: &'static str,
    relative_relocation_type: u32,
    linker_arguments: &'static [&'static str],
}

const ARCHITECTURES: &[Architecture] = &[
    Architecture {
        name: "aarch64",
        clang_target: "aarch64-none-elf",
        elf_machine: "AArch64",
        production_relocation_source: "src/arch/aarch64/relocate.rs",
        relative_relocation_name: "R_AARCH64_RELATIVE",
        relative_relocation_type: 1027,
        linker_arguments: &[],
    },
    Architecture {
        name: "riscv64",
        clang_target: "riscv64-none-elf",
        elf_machine: "RISC-V",
        production_relocation_source: "src/arch/riscv64/relocate.rs",
        relative_relocation_name: "R_RISCV_RELATIVE",
        relative_relocation_type: 3,
        linker_arguments: &["--no-relax"],
    },
    Architecture {
        name: "loongarch64",
        clang_target: "loongarch64-none-elf",
        elf_machine: "LoongArch",
        production_relocation_source: "src/arch/loongarch64/relocate.rs",
        relative_relocation_name: "R_LARCH_RELATIVE",
        relative_relocation_type: 3,
        linker_arguments: &[],
    },
];

#[test]
fn non_x86_final_elf_survives_three_runtime_load_biases() {
    assert_eq!(
        ARCHITECTURES
            .iter()
            .map(|architecture| architecture.name)
            .collect::<Vec<_>>(),
        ["aarch64", "riscv64", "loongarch64"],
        "each non-x86 production architecture needs deterministic runtime load-bias coverage"
    );

    let temporary_directory = TemporaryDirectory::new();
    for architecture in ARCHITECTURES {
        verify_runtime_load_biases(*architecture, temporary_directory.path());
    }
}

fn verify_runtime_load_biases(architecture: Architecture, temporary_root: &Path) {
    verify_production_relocation_contract(architecture);
    let architecture_directory = temporary_root.join(architecture.name);
    fs::create_dir_all(&architecture_directory)
        .expect("architecture fixture directory must be created");
    let fixture = build_fixture(architecture, &architecture_directory);
    let header = run_output(
        Command::new("readelf").args(["-hW"]).arg(&fixture),
        "inspect fixture ELF header",
    );
    assert!(
        header.contains("Type:")
            && header.contains("DYN (")
            && header.contains(architecture.elf_machine),
        "{} fixture must be a final ET_DYN image for the expected machine:\n{header}",
        architecture.name
    );

    let symbols = read_symbols(&fixture);
    let relocation_output = run_output(
        Command::new("readelf").args(["-rW"]).arg(&fixture),
        "inspect fixture relocations",
    );
    let all_relocation_offsets = relocation_offsets(&relocation_output, None);
    let relative_relocation_offsets = relocation_offsets(
        &relocation_output,
        Some(architecture.relative_relocation_name),
    );
    assert_eq!(
        relative_relocation_offsets,
        [symbol(&symbols, "target_ptr")],
        "{} fixture must contain one real relative relocation at target_ptr:\n{relocation_output}",
        architecture.name
    );
    assert_eq!(
        all_relocation_offsets, relative_relocation_offsets,
        "{} fixture must not hide a non-relative dynamic relocation:\n{relocation_output}",
        architecture.name
    );
    let percpu_range = symbol(&symbols, "percpu_start")..symbol(&symbols, "percpu_end");
    assert!(
        all_relocation_offsets
            .iter()
            .all(|offset| !percpu_range.contains(offset)),
        "{} per-CPU storage must not contain a relocation target",
        architecture.name
    );

    let image = fs::read(&fixture).expect("linked relocation fixture must be readable");
    let elf = ElfImage::parse(&image);
    let mappings = (0..RUNTIME_LOAD_COUNT)
        .map(|_| elf.load(&image, MappingPermissions::ReadWrite))
        .collect::<Vec<_>>();
    let load_biases = mappings
        .iter()
        .map(|mapping| elf.load_bias(mapping))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        load_biases.len(),
        RUNTIME_LOAD_COUNT,
        "{} fixture must use three distinct runtime load biases",
        architecture.name
    );

    for mapping in &mappings {
        apply_production_relocations(architecture, &elf, mapping, &symbols);
        verify_runtime_addresses(architecture, &elf, mapping, &symbols);
    }
}

fn build_fixture(architecture: Architecture, directory: &Path) -> PathBuf {
    let assembly_source = directory.join("fixture.S");
    fs::write(
        &assembly_source,
        include_str!("fixtures/non_x86_load_bias.S"),
    )
    .expect("fixture assembly source must be written");
    let object = directory.join("fixture.o");
    run_checked(
        Command::new("clang")
            .arg(format!("--target={}", architecture.clang_target))
            .args(["-c", "-fPIC"])
            .arg(&assembly_source)
            .arg("-o")
            .arg(&object),
        "assemble cross-architecture relocation fixture",
    );

    let linker_script = directory.join("fixture.ld");
    fs::write(
        &linker_script,
        include_str!("fixtures/non_x86_load_bias.ld"),
    )
    .expect("fixture linker script must be written");
    let fixture = directory.join(format!("{}-load-bias-fixture.elf", architecture.name));
    let mut linker = Command::new("rust-lld");
    linker.args([
        "-flavor",
        "gnu",
        "-pie",
        "--no-dynamic-linker",
        "--gc-sections",
        "--build-id=none",
        "-z",
        "norelro",
    ]);
    linker.args(architecture.linker_arguments);
    linker
        .arg("-T")
        .arg(&linker_script)
        .arg(&object)
        .arg("-o")
        .arg(&fixture);
    run_checked(&mut linker, "link final cross-architecture PIE fixture");
    fixture
}

fn verify_production_relocation_contract(architecture: Architecture) {
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(architecture.production_relocation_source),
    )
    .expect("production architecture relocation source must be readable");
    let constant = format!(
        "const {}: u32 = {};",
        architecture.relative_relocation_name, architecture.relative_relocation_type
    );
    assert!(
        source.contains(&constant),
        "{} production relocation type must match the final ELF ABI",
        architecture.name
    );
    assert!(
        source.contains("crate::elf::apply_reloc(")
            && source.contains(&format!("{},", architecture.relative_relocation_name)),
        "{} production entry must delegate its relative records to the shared RELA walker",
        architecture.name
    );
}

fn apply_production_relocations(
    architecture: Architecture,
    elf: &ElfImage,
    mapping: &elf_image::LoadedElf,
    symbols: &BTreeMap<String, usize>,
) {
    let relocation_start = elf.runtime_address(mapping, symbol(symbols, "__rela_dyn_begin"));
    let relocation_end = elf.runtime_address(mapping, symbol(symbols, "__rela_dyn_end"));
    // SAFETY: the final ELF parser bounds-checked every PT_LOAD segment. The
    // linker script aligns a complete RELA table within that private writable
    // mapping, and every relative relocation target was verified to be a
    // loaded symbol before this call.
    unsafe {
        production_elf::apply_reloc(
            elf.load_bias(mapping),
            relocation_start,
            relocation_end,
            architecture.relative_relocation_type,
        )
    };
}

fn verify_runtime_addresses(
    architecture: Architecture,
    elf: &ElfImage,
    mapping: &elf_image::LoadedElf,
    symbols: &BTreeMap<String, usize>,
) {
    let target_pointer = read_word(elf.runtime_address(mapping, symbol(symbols, "target_ptr")));
    let marker = elf.runtime_address(mapping, symbol(symbols, "marker")) as usize;
    assert_eq!(
        target_pointer, marker,
        "{} relative relocation did not follow the runtime load bias",
        architecture.name
    );

    let linked_percpu_offset = symbol(symbols, "percpu_value") - symbol(symbols, "percpu_start");
    let stored_percpu_offset =
        read_word(elf.runtime_address(mapping, symbol(symbols, "percpu_offset")));
    assert_eq!(
        stored_percpu_offset, linked_percpu_offset,
        "{} final ELF must store a load-bias-independent per-CPU offset",
        architecture.name
    );
    let runtime_percpu_value = elf
        .runtime_address(mapping, symbol(symbols, "percpu_start"))
        .wrapping_add(stored_percpu_offset);
    assert_eq!(
        runtime_percpu_value,
        elf.runtime_address(mapping, symbol(symbols, "percpu_value")),
        "{} area_base + relative_offset must resolve after relocation",
        architecture.name
    );
    assert_eq!(
        read_word(runtime_percpu_value),
        PERCPU_MAGIC,
        "{} resolved per-CPU storage must retain its initialized value",
        architecture.name
    );
}

fn read_symbols(fixture: &Path) -> BTreeMap<String, usize> {
    let output = run_output(
        Command::new("readelf").args(["-sW"]).arg(fixture),
        "inspect fixture symbols",
    );
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.get(6) == Some(&"UND") {
                return None;
            }
            let name = fields.last()?.to_string();
            let value = usize::from_str_radix(fields.get(1)?, 16).ok()?;
            Some((name, value))
        })
        .collect()
}

fn symbol(symbols: &BTreeMap<String, usize>, name: &str) -> usize {
    *symbols
        .get(name)
        .unwrap_or_else(|| panic!("fixture symbol {name:?} must exist"))
}

fn relocation_offsets(output: &str, relocation_name: Option<&str>) -> Vec<usize> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let relocation_type = *fields.get(2)?;
            if !relocation_type.starts_with("R_")
                || relocation_name.is_some_and(|expected| expected != relocation_type)
            {
                return None;
            }
            usize::from_str_radix(fields.first()?, 16).ok()
        })
        .collect()
}

fn read_word(address: *mut u8) -> usize {
    // SAFETY: fixture object symbols are eight-byte aligned, and the ELF loader
    // bounds-checked the complete word before returning this mapped address.
    unsafe { address.cast::<usize>().read() }
}

fn run_checked(command: &mut Command, operation: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to {operation}: {error}"));
    assert!(
        output.status.success(),
        "failed to {operation}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_output(command: &mut Command, operation: &str) -> String {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to {operation}: {error}"));
    assert!(
        output.status.success(),
        "failed to {operation}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("tool output must be UTF-8")
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "someboot-non-x86-relocation-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary relocation directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
