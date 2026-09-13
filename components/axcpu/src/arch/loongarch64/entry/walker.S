// Shared four-level 4 KiB walker. The caller owns t0/TLBRSAVE and the miss path.
.ifndef AX_CPU_WALK_4K_DEFINED
.set AX_CPU_WALK_4K_DEFINED, 1
.macro AX_CPU_WALK_4K invalid
    lddir $t0, $t0, 3
    beqz $t0, \invalid
    lddir $t0, $t0, 2
    beqz $t0, \invalid
    lddir $t0, $t0, 1
    beqz $t0, \invalid
    ldpte $t0, 0
    ldpte $t0, 1
.endm
.endif
