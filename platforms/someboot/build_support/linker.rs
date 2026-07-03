#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkerArch {
    Aarch64,
    Loongarch64,
    X86_64,
    Riscv64,
}

#[derive(Debug, Clone, Copy)]
pub struct LinkerConfig {
    pub kernel_load_vaddr: u64,
    pub kernel_load_paddr: u64,
}

struct LinkerTemplate {
    path: &'static str,
    content: &'static str,
    replacements: &'static [(&'static str, &'static str)],
}

const DEFAULTS_PATH: &str = "src/ld/defaults.ld";
const TEXT_PATH: &str = "src/ld/text.ld";
const RODATA_PATH: &str = "src/ld/rodata.ld";
const DATA_PATH: &str = "src/ld/data.ld";
const RELA_DYN_PATH: &str = "src/ld/rela-dyn.ld";
const BSS_PATH: &str = "src/ld/bss.ld";
const DEBUG_PATH: &str = "src/ld/debug.ld";
const DISCARD_EXIT_PATH: &str = "src/ld/discard-exit.ld";
const DISCARD_DYNAMIC_PATH: &str = "src/ld/discard-dynamic.ld";

const FRAGMENTS: &[(&str, &str, &str)] = &[
    (
        DEFAULTS_PATH,
        "${ld_defaults}",
        include_str!("../src/ld/defaults.ld"),
    ),
    (TEXT_PATH, "${ld_text}", include_str!("../src/ld/text.ld")),
    (
        RODATA_PATH,
        "${ld_rodata}",
        include_str!("../src/ld/rodata.ld"),
    ),
    (DATA_PATH, "${ld_data}", include_str!("../src/ld/data.ld")),
    (
        RELA_DYN_PATH,
        "${ld_rela_dyn}",
        include_str!("../src/ld/rela-dyn.ld"),
    ),
    (BSS_PATH, "${ld_bss}", include_str!("../src/ld/bss.ld")),
    (
        DEBUG_PATH,
        "${ld_debug}",
        include_str!("../src/ld/debug.ld"),
    ),
    (
        DISCARD_EXIT_PATH,
        "${ld_discard_exit}",
        include_str!("../src/ld/discard-exit.ld"),
    ),
    (
        DISCARD_DYNAMIC_PATH,
        "${ld_discard_dynamic}",
        include_str!("../src/ld/discard-dynamic.ld"),
    ),
];

const AARCH64_REPLACEMENTS: &[(&str, &str)] = &[
    ("${text_output}", ""),
    ("${rodata_extra}", ""),
    (
        "${data_extra}",
        r#"        *(.got .got.*)
        *(.got.plt .got.plt.*)
        *(.igot .igot.*)"#,
    ),
    ("${percpu_align}", "64"),
    ("${tdata_align}", ""),
    ("${tdata_output}", ":text :tls"),
    ("${post_tdata_sections}", ""),
    ("${edata_align}", ". = ALIGN(8);"),
    ("${bss_start_before_tbss}", "__bss_start = .;"),
    ("${bss_start_after_tbss}", ""),
    ("${tbss_align}", ""),
    ("${tbss_output}", ":text :tls"),
    ("${pre_sbss_align}", ". = ALIGN(8);"),
    ("${sbss_extra}", ""),
    ("${bss_output}", ":text"),
    ("${cpu_stack_align}", ". = ALIGN(PAGE_SIZE);"),
    ("${discard_options}", "*(.options)"),
    ("${discard_dynamic_extra}", "*(.eh_frame .eh_frame_hdr)"),
];

const LOONGARCH64_REPLACEMENTS: &[(&str, &str)] = &[
    ("${text_output}", ":text = 0"),
    ("${rodata_extra}", ""),
    ("${data_extra}", ""),
    ("${percpu_align}", "PAGE_SIZE"),
    ("${tdata_align}", ""),
    ("${tdata_output}", ":text :tls"),
    ("${post_tdata_sections}", ""),
    ("${edata_align}", ". = ALIGN(8);"),
    ("${bss_start_before_tbss}", "__bss_start = .;"),
    ("${bss_start_after_tbss}", ""),
    ("${tbss_align}", ""),
    ("${tbss_output}", ":text :tls"),
    ("${pre_sbss_align}", ". = ALIGN(8);"),
    ("${sbss_extra}", ""),
    ("${bss_output}", ""),
    ("${cpu_stack_align}", ". = ALIGN(PAGE_SIZE);"),
    ("${discard_options}", "*(.options)"),
    ("${discard_dynamic_extra}", "*(.eh_frame)"),
];

const X86_64_REPLACEMENTS: &[(&str, &str)] = &[
    ("${text_output}", ":text = 0"),
    ("${rodata_extra}", ""),
    ("${data_extra}", "*(.got .got.*)"),
    ("${percpu_align}", "PAGE_SIZE"),
    ("${tdata_align}", "ALIGN(16)"),
    ("${tdata_output}", ":text :tls"),
    ("${post_tdata_sections}", ""),
    ("${edata_align}", ""),
    ("${bss_start_before_tbss}", "__bss_start = .;"),
    ("${bss_start_after_tbss}", ""),
    ("${tbss_align}", "ALIGN(16)"),
    ("${tbss_output}", ":text :tls"),
    ("${pre_sbss_align}", ""),
    ("${sbss_extra}", ""),
    ("${bss_output}", ":text"),
    ("${cpu_stack_align}", ". = ALIGN(PAGE_SIZE);"),
    ("${discard_options}", ""),
    ("${discard_dynamic_extra}", "*(.eh_frame)"),
];

const RISCV64_REPLACEMENTS: &[(&str, &str)] = &[
    ("${text_output}", ""),
    ("${rodata_extra}", "*(.srodata .srodata.*)"),
    ("${data_extra}", ""),
    ("${percpu_align}", "PAGE_SIZE"),
    ("${tdata_align}", ""),
    ("${tdata_output}", ""),
    (
        "${post_tdata_sections}",
        r#"
    .dynamic : ALIGN(8) {
        *(.dynamic)
    }
    .got : ALIGN(8) {
        *(.got .got.*)
    }"#,
    ),
    ("${edata_align}", ""),
    ("${bss_start_before_tbss}", ""),
    ("${bss_start_after_tbss}", "__bss_start = .;"),
    ("${tbss_align}", ""),
    ("${tbss_output}", ""),
    ("${pre_sbss_align}", ""),
    ("${sbss_extra}", "*(.sbss.*)"),
    ("${bss_output}", ""),
    ("${cpu_stack_align}", ""),
    ("${discard_options}", ""),
    ("${discard_dynamic_extra}", ""),
];

pub fn render_linker_script(arch: LinkerArch, config: LinkerConfig) -> String {
    let template = arch.template();
    let mut script = template.content.to_string();

    for (_, token, fragment) in FRAGMENTS {
        script = script.replace(token, fragment.trim_end());
    }
    for (token, value) in template.replacements {
        script = script.replace(token, value);
    }

    script
        .replace(
            "${kernel_load_vaddr}",
            &format!("{:#x}", config.kernel_load_vaddr as usize),
        )
        .replace(
            "${kernel_load_paddr}",
            &format!("{:#x}", config.kernel_load_paddr as usize),
        )
}

pub fn source_paths() -> Vec<&'static str> {
    let mut paths = vec![
        "build_support/linker.rs",
        LinkerArch::Aarch64.template().path,
        LinkerArch::Loongarch64.template().path,
        LinkerArch::X86_64.template().path,
        LinkerArch::Riscv64.template().path,
    ];
    paths.extend(FRAGMENTS.iter().map(|(path, ..)| *path));
    paths
}

impl LinkerArch {
    fn template(self) -> LinkerTemplate {
        match self {
            LinkerArch::Aarch64 => LinkerTemplate {
                path: "src/arch/aarch64/link.ld",
                content: include_str!("../src/arch/aarch64/link.ld"),
                replacements: AARCH64_REPLACEMENTS,
            },
            LinkerArch::Loongarch64 => LinkerTemplate {
                path: "src/arch/loongarch64/link.ld",
                content: include_str!("../src/arch/loongarch64/link.ld"),
                replacements: LOONGARCH64_REPLACEMENTS,
            },
            LinkerArch::X86_64 => LinkerTemplate {
                path: "src/arch/x86_64/link.ld",
                content: include_str!("../src/arch/x86_64/link.ld"),
                replacements: X86_64_REPLACEMENTS,
            },
            LinkerArch::Riscv64 => LinkerTemplate {
                path: "src/arch/riscv64/link.ld",
                content: include_str!("../src/arch/riscv64/link.ld"),
                replacements: RISCV64_REPLACEMENTS,
            },
        }
    }
}
