    .data : ALIGN(16) {
        *(.data .data.*)
        *(.init.data)
        *(.sdata .sdata.*)
        ${data_extra}

        . = ALIGN(${percpu_align});
        __percpu_start = .;
        *(.percpu_data .percpu_data.* .percpu .percpu.*)
        __percpu_end = .;
    }
