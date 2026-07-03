    /DISCARD/ : {
        *(.dynamic .dynsym .dynstr .hash .gnu.hash)
        *(.gnu.attributes)
        ${discard_options}
        ${discard_dynamic_extra}
    }
