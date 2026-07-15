#![cfg(target_arch = "x86_64")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const FIXTURE_LINK_BASE: u64 = 0xffff_ffff_8000_0000;
const PAGE_SIZE: usize = 4096;

#[test]
fn raw_x86_entry_relocates_in_naked_pic_code_before_rust() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let head = fs::read_to_string(manifest_dir.join("src/arch/x86_64/head.rs"))
        .expect("x86 head source must be readable");
    let entry = fs::read_to_string(manifest_dir.join("src/arch/x86_64/entry.rs"))
        .expect("x86 entry source must be readable");
    let linker = fs::read_to_string(manifest_dir.join("src/arch/x86_64/link.ld"))
        .expect("x86 linker template must be readable");

    let raw_entry = function_body(&head, "pub unsafe extern \"C\" fn x86_64_raw_entry(");
    assert!(
        head.contains(
            "#[unsafe(naked)]\n#[unsafe(no_mangle)]\n#[unsafe(link_section = \
             \".head.text.100.raw_entry\")]\npub unsafe extern \"C\" fn x86_64_raw_entry("
        ),
        "the raw image must enter a naked function before any Rust prologue"
    );
    for required in [
        "lea r8, [rip + {head}]",
        "__rela_dyn_begin",
        "__rela_dyn_end",
        "relative_relocation = const 8_u32",
        "jmp {rust_entry}",
    ] {
        assert!(
            raw_entry.contains(required),
            "raw x86 relocation entry is missing {required:?}"
        );
    }
    assert!(
        !raw_entry.contains("call "),
        "raw relocation must not call Rust or use a return stack"
    );
    assert!(
        !function_body(&entry, "pub extern \"C\" fn kernel_entry(").contains("relocate::relocate"),
        "ordinary Rust entry is already too late to apply its own relocations"
    );
    assert!(
        linker.contains("ENTRY(x86_64_raw_entry)"),
        "ELF loaders must enter the relocation trampoline rather than the image header"
    );
    assert!(
        linker.contains(
            "_kernel_entry = ABSOLUTE(KERNEL_LOAD_ADDRESS + (x86_64_raw_entry - _head));"
        ),
        "the raw loader entry must be a physical load-base-relative trampoline"
    );

    let archive = find_someboot_archive();
    let output = Command::new("objdump")
        .args(["-dr", "--disassemble=x86_64_raw_entry"])
        .arg(&archive)
        .output()
        .unwrap_or_else(|error| panic!("failed to disassemble {}: {error}", archive.display()));
    assert!(
        output.status.success(),
        "objdump failed for {}: {}",
        archive.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let disassembly = String::from_utf8(output.stdout).expect("objdump output must be UTF-8");
    assert!(
        disassembly.contains("<x86_64_raw_entry>:")
            && disassembly.contains("R_X86_64_PC32")
            && disassembly.contains("__rela_dyn_begin")
            && disassembly.contains("__rela_dyn_end")
            && disassembly.contains("kernel_entry"),
        "compiled trampoline must retain only PC-relative symbol transfers:\n{disassembly}"
    );
    assert!(
        !disassembly.lines().any(|line| line.contains("\tcall")),
        "compiled raw trampoline must not contain a call instruction:\n{disassembly}"
    );

    verify_three_runtime_load_biases(&archive);
}

fn verify_three_runtime_load_biases(archive: &Path) {
    let temp = std::env::temp_dir().join(format!(
        "someboot-x86-relocation-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).expect("temporary relocation fixture directory must be created");

    let full_object = temp.join("someboot-head.o");
    extract_archive_member_with_symbol(archive, "x86_64_raw_entry", &full_object);
    let fixture_object = temp.join("someboot-head-fixture.o");
    run_checked(
        Command::new("objcopy")
            .arg("--remove-relocations=.head.text.000.header")
            .arg(&full_object)
            .arg(&fixture_object),
        "remove non-executed image-header relocations from the loader fixture",
    );

    let stub_source = temp.join("fixture.S");
    fs::write(
        &stub_source,
        r#"
        .section .text.fixture,"ax",@progbits
        .globl kernel_entry
        .type kernel_entry,@function
kernel_entry:
        lea marker(%rip), %rdx
        mov target_ptr(%rip), %rax
        cmp %rdx, %rax
        sete %al
        movzbq %al, %rax
        ret

        .globl __x86_64_efi_pe_entry
        .type __x86_64_efi_pe_entry,@function
__x86_64_efi_pe_entry:
        ret

        .section .data.fixture,"aw",@progbits
        .p2align 3
target_ptr:
        .quad marker
        .hidden marker
marker:
        .quad 0x5a5aa5a5
"#,
    )
    .expect("fixture assembly must be written");
    let stub_object = temp.join("fixture.o");
    run_checked(
        Command::new("cc")
            .args(["-c", "-fPIC"])
            .arg(&stub_source)
            .arg("-o")
            .arg(&stub_object),
        "assemble relocation fixture",
    );

    let linker_script = temp.join("fixture.ld");
    fs::write(
        &linker_script,
        format!(
            r#"
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(x86_64_raw_entry)
PHDRS {{
    image PT_LOAD FLAGS(7);
    dynamic PT_DYNAMIC FLAGS(6);
}}
SECTIONS {{
    . = {FIXTURE_LINK_BASE:#x};
    .head.text : {{
        *(.head.text.000.header)
        *(.head.text.100.raw_entry)
    }} :image
    .text : {{ *(.text.fixture) }} :image
    .rodata : {{ *(.rodata*) }} :image
    .data : {{ *(.data.fixture) *(.got .got.*) }} :image
    .dynamic : {{ *(.dynamic) }} :image :dynamic
    .rela.dyn : {{
        __rela_dyn_begin = .;
        *(.rela.dyn)
    }} :image
    __rela_dyn_end = ADDR(.rela.dyn) + SIZEOF(.rela.dyn);
    _end = .;

    PAGE_SIZE = 0x1000;
    PECOFF_FILE_ALIGN = 0x200;
    _kernel_entry = x86_64_raw_entry;
    _kernel_asize = _end - _head;
    _kernel_code_size = SIZEOF(.text);
    _kernel_rsize = SIZEOF(.data);
    _kernel_bss_size = 0;
    _kernel_text_offset = ADDR(.text) - _head;
    _kernel_image_size = _end - _head;
    _etext = ADDR(.data);
}}
"#
        ),
    )
    .expect("fixture linker script must be written");
    let fixture = temp.join("raw-entry-fixture.elf");
    run_checked(
        Command::new("ld")
            .args([
                "-pie",
                "--no-dynamic-linker",
                "--gc-sections",
                "-z",
                "norelro",
            ])
            .arg("-T")
            .arg(&linker_script)
            .arg(&fixture_object)
            .arg(&stub_object)
            .arg("-o")
            .arg(&fixture),
        "link the executable relocation fixture",
    );

    let relocations = run_output(
        Command::new("readelf").args(["-rW"]).arg(&fixture),
        "read fixture relocations",
    );
    assert!(
        relocations.contains("R_X86_64_RELATIVE"),
        "fixture must exercise a real ELF relative relocation:\n{relocations}"
    );

    let image = fs::read(&fixture).expect("linked relocation fixture must be readable");
    let elf = ElfImage::parse(&image);
    let mut mappings = Vec::new();
    for _ in 0..3 {
        mappings.push(elf.load(&image));
    }
    let biases = mappings
        .iter()
        .map(|mapping| (mapping.base as usize).wrapping_sub(elf.link_base))
        .collect::<Vec<_>>();
    assert_eq!(
        biases
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "the loader fixture must exercise three distinct runtime slides"
    );
    for mapping in &mappings {
        // SAFETY: ElfImage::load copied executable PT_LOAD bytes into this
        // private RWX mapping and the entry offset was bounds-checked.
        let entry: unsafe extern "C" fn() -> usize =
            unsafe { core::mem::transmute(mapping.base.add(elf.entry_offset)) };
        // SAFETY: the fixture entry has the declared no-argument ABI and its
        // terminal stub returns normally after checking the relocated pointer.
        assert_eq!(
            unsafe { entry() },
            1,
            "relocation failed at a runtime slide"
        );
    }

    drop(mappings);
    fs::remove_dir_all(&temp).expect("temporary relocation fixture must be removable");
}

fn extract_archive_member_with_symbol(archive: &Path, symbol: &str, output: &Path) {
    let members = run_output(
        Command::new("ar").arg("t").arg(archive),
        "list someboot archive members",
    );
    for member in members.lines().filter(|member| !member.is_empty()) {
        let extracted = Command::new("ar")
            .arg("p")
            .arg(archive)
            .arg(member)
            .output()
            .expect("archive member extraction must start");
        assert!(extracted.status.success(), "failed to extract {member}");
        fs::write(output, extracted.stdout).expect("archive member must be written");
        let symbols = run_output(
            Command::new("nm").arg(output),
            "inspect someboot archive member",
        );
        if symbols.contains(symbol) {
            return;
        }
    }
    panic!(
        "no archive member in {} defines {symbol}",
        archive.display()
    );
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

#[derive(Clone, Copy)]
struct LoadSegment {
    file_offset: usize,
    virtual_address: u64,
    file_size: usize,
    memory_size: usize,
}

struct ElfImage {
    link_base: usize,
    entry_offset: usize,
    mapping_size: usize,
    segments: Vec<LoadSegment>,
}

impl ElfImage {
    fn parse(image: &[u8]) -> Self {
        assert_eq!(&image[..4], b"\x7fELF", "fixture must be an ELF image");
        assert_eq!(image[4], 2, "fixture must use ELF64");
        let entry = read_u64(image, 24);
        let program_offset = usize::try_from(read_u64(image, 32)).unwrap();
        let program_size = usize::from(read_u16(image, 54));
        let program_count = usize::from(read_u16(image, 56));
        assert_eq!(program_size, 56, "fixture must use native ELF64 phdrs");

        let mut segments = Vec::new();
        for index in 0..program_count {
            let offset = program_offset + index * program_size;
            if read_u32(image, offset) != 1 {
                continue;
            }
            let segment = LoadSegment {
                file_offset: usize::try_from(read_u64(image, offset + 8)).unwrap(),
                virtual_address: read_u64(image, offset + 16),
                file_size: usize::try_from(read_u64(image, offset + 32)).unwrap(),
                memory_size: usize::try_from(read_u64(image, offset + 40)).unwrap(),
            };
            assert!(segment.file_size <= segment.memory_size);
            assert!(segment.file_offset + segment.file_size <= image.len());
            segments.push(segment);
        }
        assert!(!segments.is_empty(), "fixture must contain PT_LOAD");
        let link_base = segments
            .iter()
            .map(|segment| segment.virtual_address as usize & !(PAGE_SIZE - 1))
            .min()
            .unwrap();
        let link_end = segments
            .iter()
            .map(|segment| usize::try_from(segment.virtual_address).unwrap() + segment.memory_size)
            .max()
            .unwrap();
        let mapping_size = align_up(link_end.wrapping_sub(link_base), PAGE_SIZE);
        let entry_offset = usize::try_from(entry).unwrap().wrapping_sub(link_base);
        assert!(entry_offset < mapping_size, "ELF entry must lie in PT_LOAD");
        Self {
            link_base,
            entry_offset,
            mapping_size,
            segments,
        }
    }

    fn load(&self, image: &[u8]) -> ExecutableMapping {
        // SAFETY: arguments request a new private anonymous mapping. The return
        // value is checked against MAP_FAILED before use.
        let base = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                self.mapping_size,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(base, libc::MAP_FAILED, "fixture mmap must succeed");
        let base = base.cast::<u8>();
        for segment in &self.segments {
            let destination_offset = usize::try_from(segment.virtual_address)
                .unwrap()
                .wrapping_sub(self.link_base);
            assert!(destination_offset + segment.memory_size <= self.mapping_size);
            // SAFETY: both source and destination ranges were bounds-checked;
            // each destination lies in this fresh private mapping.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image.as_ptr().add(segment.file_offset),
                    base.add(destination_offset),
                    segment.file_size,
                );
                base.add(destination_offset + segment.file_size)
                    .write_bytes(0, segment.memory_size - segment.file_size);
            }
        }
        ExecutableMapping {
            base,
            size: self.mapping_size,
        }
    }
}

struct ExecutableMapping {
    base: *mut u8,
    size: usize,
}

impl Drop for ExecutableMapping {
    fn drop(&mut self) {
        // SAFETY: this is the exact live mapping returned by mmap and owned by
        // this value; no loaded function is running during drop.
        assert_eq!(unsafe { libc::munmap(self.base.cast(), self.size) }, 0);
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn align_up(value: usize, alignment: usize) -> usize {
    value
        .checked_add(alignment - 1)
        .expect("fixture size must not overflow")
        & !(alignment - 1)
}

fn find_someboot_archive() -> PathBuf {
    let deps = std::env::current_exe()
        .expect("test executable path must be available")
        .parent()
        .expect("test executable must live in a dependency directory")
        .to_path_buf();
    let mut candidates = fs::read_dir(&deps)
        .expect("dependency directory must be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libsomeboot-") && name.ends_with(".rlib"))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        path.metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    candidates
        .into_iter()
        .rev()
        .find(|archive| {
            Command::new("nm")
                .arg(archive)
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).contains("x86_64_raw_entry")
                })
        })
        .unwrap_or_else(|| {
            panic!(
                "no someboot archive with raw x86 entry found in {}",
                deps.display()
            )
        })
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let function_start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature {signature:?}"));
    let body_start = source[function_start..]
        .find('{')
        .map(|offset| function_start + offset)
        .expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in source[body_start..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body for {signature:?}")
}
