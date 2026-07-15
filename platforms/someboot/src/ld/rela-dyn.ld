    .rela.dyn : ALIGN(8) {
        __rela_dyn_begin = .;
        *(.rela .rela*)
    }
    /* GNU ld may synthesize dynamic relocations after evaluating assignments
       inside the output section. Derive the final bound from section metadata
       so raw entry observes those linker-created records as well. */
    __rela_dyn_end = ADDR(.rela.dyn) + SIZEOF(.rela.dyn);
