/* Each bank points to 32*vlenb live, exclusive bytes. t0 selects its header;
 * t1, t2 and t4 are scratch. Whole-register transfers must start at zero even
 * when an interrupted vector operation left vstart nonzero. */
.macro PUSH_VECTOR_REGS
    .option push
    .option arch, +v
    csrr t1, vstart
    sd   t1, {vector_vstart}(t0)
    csrr t1, vl
    sd   t1, {vector_vl}(t0)
    csrr t1, vtype
    sd   t1, {vector_vtype}(t0)
    csrr t1, vcsr
    sd   t1, {vector_vcsr}(t0)
    csrw vstart, zero
    ld   t2, {vector_registers}(t0)
    csrr t4, vlenb
    slli t4, t4, 3
    vs8r.v v0, (t2)
    add  t2, t2, t4
    vs8r.v v8, (t2)
    add  t2, t2, t4
    vs8r.v v16, (t2)
    add  t2, t2, t4
    vs8r.v v24, (t2)
    .option pop
.endm

.macro POP_VECTOR_REGS
    .option push
    .option arch, +v
    csrw vstart, zero
    ld   t2, {vector_registers}(t0)
    csrr t4, vlenb
    slli t4, t4, 3
    vl8re8.v v0, (t2)
    add  t2, t2, t4
    vl8re8.v v8, (t2)
    add  t2, t2, t4
    vl8re8.v v16, (t2)
    add  t2, t2, t4
    vl8re8.v v24, (t2)
    ld   t1, {vector_vl}(t0)
    ld   t2, {vector_vtype}(t0)
    vsetvl zero, t1, t2
    ld   t1, {vector_vcsr}(t0)
    csrw vcsr, t1
    ld   t1, {vector_vstart}(t0)
    csrw vstart, t1
    .option pop
.endm
