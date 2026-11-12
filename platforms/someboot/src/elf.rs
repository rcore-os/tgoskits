use core::ops::Range;

// RELA 重定位结构 (参考 include/uapi/linux/elf.h)
#[repr(C)]
pub struct Rela {
    pub r_offset: u64, // 需要重定位的地址
    pub r_info: u64,   // 类型和符号索引
    pub r_addend: i64, // 加数值
}

impl Rela {
    #[inline]
    fn r_type_raw(&self) -> u32 {
        (self.r_info & 0xFFFFFFFF) as u32
    }
}

/// Validates the complete linker table before applying any relative relocation.
/// Returns `false` without writing when a record violates the image bounds.
///
/// # Safety
/// `start..end` must refer to readable initialized storage in the live image.
/// `writable` must be the image's exclusively owned writable storage, obtained
/// from running linker addresses, not from uploaded ELF metadata. The table
/// and destinations must remain stable and exclusively owned until return.
/// In particular, `writable` must exclude the active stack and boot record.
#[must_use]
pub unsafe fn apply_reloc(
    load_offset: i128,
    start: *mut u8,
    end: *const u8,
    r_type: u32,
    writable: Range<usize>,
) -> bool {
    let table = start as usize..end as usize;
    let Some(bytes) = table.end.checked_sub(table.start) else {
        return false;
    };
    if writable.start > writable.end
        || !table.start.is_multiple_of(align_of::<Rela>())
        || !bytes.is_multiple_of(size_of::<Rela>())
        || bytes > isize::MAX as usize
    {
        return false;
    }
    if bytes == 0 {
        return true;
    }
    // SAFETY: the caller supplies live readable table storage; the checks
    // above establish alignment, complete records and a representable length.
    let relocations =
        unsafe { core::slice::from_raw_parts(start.cast::<Rela>(), bytes / size_of::<Rela>()) };

    for reloc in relocations {
        if reloc.r_type_raw() == 0 {
            continue;
        }
        let Ok(target) = usize::try_from(reloc.r_offset as i128 + load_offset) else {
            return false;
        };
        let Some(target_end) = target.checked_add(size_of::<usize>()) else {
            return false;
        };
        if reloc.r_info != u64::from(r_type)
            || !target.is_multiple_of(align_of::<usize>())
            || target < writable.start
            || target_end > writable.end
            || target < table.end && table.start < target_end
        {
            return false;
        }
    }

    for reloc in relocations {
        if reloc.r_type_raw() == r_type {
            let addr = (reloc.r_offset as i128 + load_offset) as usize as *mut usize;
            let val = (reloc.r_addend as i128 + load_offset) as usize;
            // SAFETY: the complete first pass proved this aligned write lies
            // in caller-owned storage and cannot alter any table record.
            unsafe { *addr = val };
        }
    }
    true
}

/// 应用 .rela.dyn 重定位
/// # Safety
/// 此函数操作裸指针，调用者必须确保传入的指针范围有效且指向合法的 RELA 重定位表。
pub unsafe fn reset(r_type: u32) {
    unsafe extern "C" {
        fn __rela_dyn_begin();
        fn __rela_dyn_end();
    }
    let start = __rela_dyn_begin as *mut u8;
    let end = __rela_dyn_end as *const u8;

    let num_entries = (end as usize - start as usize) / size_of::<Rela>();
    let relocations = unsafe { core::slice::from_raw_parts_mut(start as *mut Rela, num_entries) };
    for reloc in relocations {
        if reloc.r_type_raw() == r_type {
            let addr = reloc.r_offset as usize as *mut usize;
            unsafe { addr.write_volatile(reloc.r_addend as u64 as usize) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocation_application_is_bounded_and_validates_before_writing() {
        let mut destination = [0usize; 2];
        let mut outside = 0usize;
        let base = destination.as_mut_ptr() as usize;
        let mut records = [
            Rela {
                r_offset: base as u64,
                r_info: 3,
                r_addend: 123,
            },
            Rela {
                r_offset: (&raw mut outside) as usize as u64,
                r_info: 3,
                r_addend: 456,
            },
        ];
        let start = records.as_mut_ptr().cast::<u8>();
        let end = start.wrapping_add(size_of_val(&records));
        // SAFETY: The complete table and both potential write destinations
        // are live, uniquely owned allocations. The second destination is
        // deliberately outside the image's declared writable range.
        let applied =
            unsafe { apply_reloc(0, start, end, 3, base..base + size_of_val(&destination)) };
        assert!(!applied);
        assert_eq!(
            destination, [0; 2],
            "invalid tables must not partially apply"
        );
        assert_eq!(outside, 0, "writes must stay inside the image");

        records[1].r_offset = (base + size_of::<usize>()) as u64;
        // SAFETY: the same owned table now targets only destination's words.
        assert!(unsafe { apply_reloc(0, start, end, 3, base..base + size_of_val(&destination)) });
        assert_eq!(destination, [123, 456]);

        for info in [4, (1 << 32) | 3] {
            destination = [0; 2];
            records[1].r_info = info;
            // SAFETY: the table and storage are unchanged; unsupported types
            // and symbol references must fail before the first valid write.
            assert!(!unsafe {
                apply_reloc(0, start, end, 3, base..base + size_of_val(&destination))
            });
            assert_eq!(destination, [0; 2]);
        }

        records[0].r_info = 0;
        records[1].r_info = 3;
        records[1].r_offset = start as usize as u64;
        // SAFETY: the table is owned writable storage. Even when it is within
        // the permitted allocation, relocation must not overwrite records.
        assert!(!unsafe { apply_reloc(0, start, end, 3, start as usize..end as usize) });
        assert_eq!(records[0].r_offset, base as u64);
    }
    #[cfg(all(
        target_os = "linux",
        target_pointer_width = "64",
        target_endian = "little"
    ))]
    mod load_bias {
        extern crate std;

        use std::{
            collections::{BTreeMap, BTreeSet},
            fs,
            path::{Path, PathBuf},
            process::Command,
            string::{String, ToString},
            time::{SystemTime, UNIX_EPOCH},
            vec::Vec,
        };

        mod elf_image {
            use std::vec::Vec;

            use super::std;
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/elf_image.rs"
            ));
        }
        use elf_image::{ElfImage, MappingPermissions};

        use super::super as production_elf;

        const PERCPU_MAGIC: usize = 0x5a5a_a5a5_1357_2468;
        const RUNTIME_LOAD_COUNT: usize = 3;

        #[derive(Clone, Copy)]
        struct Architecture {
            name: &'static str,
            clang_target: &'static str,
            elf_machine: &'static str,
            relative_relocation_name: &'static str,
            relative_relocation_type: u32,
            linker_arguments: &'static [&'static str],
        }

        const ARCHITECTURES: &[Architecture] = &[
            Architecture {
                name: "aarch64",
                clang_target: "aarch64-none-elf",
                elf_machine: "AArch64",
                relative_relocation_name: "R_AARCH64_RELATIVE",
                relative_relocation_type: 1027,
                linker_arguments: &[],
            },
            Architecture {
                name: "riscv64",
                clang_target: "riscv64-none-elf",
                elf_machine: "RISC-V",
                relative_relocation_name: "R_RISCV_RELATIVE",
                relative_relocation_type: 3,
                linker_arguments: &["--no-relax"],
            },
            Architecture {
                name: "loongarch64",
                clang_target: "loongarch64-none-elf",
                elf_machine: "LoongArch",
                relative_relocation_name: "R_LARCH_RELATIVE",
                relative_relocation_type: 3,
                linker_arguments: &[],
            },
        ];

        #[test]
        fn non_x86_final_elf_survives_three_runtime_load_biases() {
            let temporary_directory = TemporaryDirectory::new();
            for architecture in ARCHITECTURES {
                verify_runtime_load_biases(*architecture, temporary_directory.path());
            }
        }

        fn verify_runtime_load_biases(architecture: Architecture, temporary_root: &Path) {
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
                "{} fixture must contain one real relative relocation at \
                 target_ptr:\n{relocation_output}",
                architecture.name
            );
            assert_eq!(
                all_relocation_offsets, relative_relocation_offsets,
                "{} fixture must not hide a non-relative dynamic relocation:\n{relocation_output}",
                architecture.name
            );
            let template_range = symbol(&symbols, "__CPU_LOCAL_AREA_PREFIX")
                ..symbol(&symbols, "__CPU_LOCAL_TEMPLATE_END");
            assert!(
                all_relocation_offsets
                    .iter()
                    .all(|offset| !template_range.contains(offset)),
                "{} CPU-local template must not contain a relocation target",
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
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/non_x86_load_bias.S"
                )),
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
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/non_x86_load_bias.ld"
                )),
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

        fn apply_production_relocations(
            architecture: Architecture,
            elf: &ElfImage,
            mapping: &elf_image::LoadedElf,
            symbols: &BTreeMap<String, usize>,
        ) {
            let relocation_start =
                elf.runtime_address(mapping, symbol(symbols, "__rela_dyn_begin"));
            let relocation_end = elf.runtime_address(mapping, symbol(symbols, "__rela_dyn_end"));
            // SAFETY: the final ELF parser bounds-checked every PT_LOAD segment. The
            // linker script aligns a complete RELA table within that private writable
            // mapping, and every relative relocation target was verified to be a
            // loaded symbol before this call.
            let applied = unsafe {
                production_elf::apply_reloc(
                    elf.load_bias(mapping),
                    relocation_start,
                    relocation_end,
                    architecture.relative_relocation_type,
                    mapping.base_address()
                        ..elf.runtime_address(mapping, symbol(symbols, "_end")) as usize,
                )
            };
            assert!(applied, "valid fixture relocation must be applied");
        }

        fn verify_runtime_addresses(
            architecture: Architecture,
            elf: &ElfImage,
            mapping: &elf_image::LoadedElf,
            symbols: &BTreeMap<String, usize>,
        ) {
            let target_pointer =
                read_word(elf.runtime_address(mapping, symbol(symbols, "target_ptr")));
            let marker = elf.runtime_address(mapping, symbol(symbols, "marker")) as usize;
            assert_eq!(
                target_pointer, marker,
                "{} relative relocation did not follow the runtime load bias",
                architecture.name
            );

            let linked_template_offset = symbol(symbols, "cpu_local_template_value")
                - symbol(symbols, "__CPU_LOCAL_AREA_PREFIX");
            let stored_template_offset = read_word(
                elf.runtime_address(mapping, symbol(symbols, "cpu_local_template_offset")),
            );
            assert_eq!(
                stored_template_offset, linked_template_offset,
                "{} final ELF must store a load-bias-independent per-CPU offset",
                architecture.name
            );
            let runtime_template_value = elf
                .runtime_address(mapping, symbol(symbols, "__CPU_LOCAL_AREA_PREFIX"))
                .wrapping_add(stored_template_offset);
            assert_eq!(
                runtime_template_value,
                elf.runtime_address(mapping, symbol(symbols, "cpu_local_template_value")),
                "{} area_base + relative_offset must resolve after relocation",
                architecture.name
            );
            assert_eq!(
                read_word(runtime_template_value),
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
    }
}
