    /DISCARD/ : {
        *(.exit.text)
        *(.text.exit)
        *(.exit.data .exit.data.*)
        *(.fini_array .fini_array.*)
        *(.dtors .dtors.*)
        *(.exitcall.exit)
        *(.discard)
        *(.discard.*)
        *(.export_symbol)
        *(.no_trim_symbol)
        *(.modinfo)
        *(.gnu.version*)
    }
