/* Linker script for StarryOS loadable kernel modules (.ko).
 *
 * Ported from `Starry-OS/StarryOS:ebpf-kmod` `modules/kmod-linker.ld`.
 * The script is consumed by `ld -r -T` (partial relink) from the
 * `cargo xtask starry kmod build` driver to produce a `.ko` image from
 * the module crate's rlib.
 */
SECTIONS
{
    . = 0x00000000;

    .modinfo : {
        KEEP(*(.modinfo))
    }

    .text : {
        *(.text .text.*)
        *(.ltext .ltext.*)
        KEEP(*(.text.init))
        KEEP(*(.text.exit))
    }

    .rodata : {
        *(.rodata .rodata.*)
        *(.lrodata .lrodata.*)
        . = ALIGN(8);
    }

    .gnu.linkonce.this_module : {
        KEEP(*(.gnu.linkonce.this_module))
    }

    .data : {
        *(.data .data.*)
    }

    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }

    .plt     : { BYTE(0) }
    .got     : { BYTE(0) }
    .got.plt : { BYTE(0) }
    .plt.idx : { BYTE(0) }

    /DISCARD/ : {
        *(.comment)
        *(.note.GNU-stack)
        *(.gnu.version)
        *(.eh_frame)
        *(.eh_frame_hdr)
    }
}
