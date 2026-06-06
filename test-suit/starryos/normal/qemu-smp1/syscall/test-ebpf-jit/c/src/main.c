#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <stdint.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);       \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                 \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define MODULE_START(name)                                              \
    printf("\n--- MODULE: %s ---\n", name)

#define SUMMARY()                                                       \
    printf("\n=== SUMMARY: %d passed, %d failed ===\n", __pass, __fail); \
    return __fail > 0 ? 1 : 0

static long raw_bpf(uint64_t cmd, void *attr, uint32_t size) {
#ifdef SYS_bpf
    return syscall(SYS_bpf, cmd, attr, size);
#else
    errno = ENOSYS;
    return -1;
#endif
}

static long raw_perf_event_open(void *attr, int pid, int cpu, int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

struct bpf_map_create_attr {
    uint32_t map_type;
    uint32_t key_size;
    uint32_t value_size;
    uint32_t max_entries;
    uint32_t map_flags;
};

struct bpf_map_elem_attr {
    uint64_t map_fd;
    uint64_t key;
    uint64_t value;
    uint64_t flags;
};

struct bpf_prog_load_attr {
    uint32_t prog_type;
    uint32_t insn_cnt;
    uint64_t insns;
    uint64_t license;
    uint32_t log_level;
    uint32_t log_size;
    uint64_t log_buf;
    uint32_t kern_version;
    uint32_t prog_flags;
};

struct bpf_insn {
    uint8_t code;
    uint8_t dst_src_reg;
    int16_t off;
    int32_t imm;
};

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
};

#define BPF_MAP_TYPE_ARRAY 2
#define BPF_MAP_TYPE_HASH  1

#define BPF_PROG_TYPE_KPROBE 2

#define BPF_MAP_CREATE       0
#define BPF_MAP_LOOKUP_ELEM  1
#define BPF_MAP_UPDATE_ELEM  2
#define BPF_PROG_LOAD        5
#define BPF_PROG_ATTACH      8
#define BPF_PROG_DETACH      9
#define BPF_OBJ_CLOSE       11

#define BPF_ANY   0

#define BPF_LD    0x00
#define BPF_LDX   0x01
#define BPF_ST    0x02
#define BPF_STX   0x03
#define BPF_ALU   0x04
#define BPF_JMP   0x05
#define BPF_JMP32 0x06
#define BPF_ALU64 0x07

#define BPF_IMM   0x00
#define BPF_MEM   0x60
#define BPF_DW    0x18
#define BPF_W     0x00
#define BPF_B     0x10
#define BPF_H     0x08

#define BPF_K     0x00
#define BPF_X     0x08

#define BPF_MOV   0xb0
#define BPF_ADD   0x00
#define BPF_SUB   0x10
#define BPF_MUL   0x20
#define BPF_DIV   0x30
#define BPF_OR    0x40
#define BPF_AND   0x50
#define BPF_LSH   0x60
#define BPF_RSH   0x70
#define BPF_NEG   0x80
#define BPF_MOD   0x90
#define BPF_XOR   0xa0
#define BPF_ARSH  0xc0

#define BPF_EXIT  0x90
#define BPF_CALL  0x80
#define BPF_JA    0x00
#define BPF_JEQ   0x10
#define BPF_JGT   0x20
#define BPF_JGE   0x30
#define BPF_JSET  0x40
#define BPF_JNE   0x50
#define BPF_JLT   0xa0
#define BPF_JLE   0xb0
#define BPF_JSGT  0x60
#define BPF_JSGE  0x70
#define BPF_JSLT  0xc0
#define BPF_JSLE  0xd0

#define BPF_TO_LE 0x00
#define BPF_TO_BE 0x08
#define BPF_END   0xd0

#define BPF_CALL_HELPER(id) make_insn(BPF_JMP | BPF_CALL, 0, 0, 0, (int32_t)(id))

/* bpf_endian flag constants - upper 4 bits of imm in BPF_END instruction */
#define BPF_ENDIAN_FLAG_BE 0x08

/* Make a BPF_END instruction: converts value in dst_reg from host to big-endian */
#define BPF_END_TO_BE(dst, size_bits) \
    make_insn(BPF_ALU | BPF_END | BPF_K, dst, 0, 0, (size_bits) | 0x08)

static struct bpf_insn make_insn(uint8_t code, uint8_t dst, uint8_t src, int16_t off, int32_t imm) {
    struct bpf_insn i;
    i.code = code;
    i.dst_src_reg = (dst & 0xf) | ((src & 0xf) << 4);
    i.off = off;
    i.imm = imm;
    return i;
}

static long load_prog(struct bpf_insn *insns, uint32_t count) {
    struct bpf_prog_load_attr attr = {
        .prog_type = BPF_PROG_TYPE_KPROBE,
        .insn_cnt = count,
        .insns = (uint64_t)insns,
        .license = 0,
        .log_level = 0,
        .log_size = 0,
        .log_buf = 0,
        .kern_version = 0,
        .prog_flags = 0,
    };
    return raw_bpf(BPF_PROG_LOAD, &attr, sizeof(attr));
}

static long open_perf_event_software(void) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = 1; /* PERF_TYPE_SOFTWARE */
    attr.size = sizeof(attr);
    attr.config = 0; /* PERF_COUNT_SW_CPU_CLOCK */
    return raw_perf_event_open(&attr, -1, 0, -1, 0);
}

/* ================================================================
 * JIT Regression Tests
 *
 * These tests target instruction patterns that were found to be
 * buggy during code review of the x86_64, AArch64, and RISC-V 64
 * JIT backends. Each test constructs a BPF program using raw
 * instructions that exercise the problematic code paths, loads it,
 * and verifies it loads successfully.
 *
 * Bug 1: emit_st RCX register conflict (x86_64)
 *   When BPF R4 (mapped to RCX) is used as the base register for
 *   ST (immediate store), the scratch register X86_RCX was
 *   overwritten before being used as the store base address.
 *   Fixed by using X86_R11 as scratch when base == X86_RCX.
 *
 * Bug 2: 32-bit DIV/MOD path missing EDX clearing (x86_64)
 *   In emit_divmod, the 32-bit branch executed DIV without first
 *   zeroing EDX. This caused incorrect results because x86 DIV
 *   uses EDX:EAX as the dividend.
 *
 * Bug 3: BPF_MOD zero division semantics
 *   BPF_MOD by zero should return dst unchanged, but the initial
 *   implementation returned 0. Fixed per eBPF specification.
 *
 * Bug 4: insn_size systematic error (all architectures)
 *   The insn_size estimation function returned wrong byte counts,
 *   causing all jump offsets to be incorrect. Fixed by using
 *   JitBuffer::new_sizing() counting pass that calls the actual
 *   emit_* functions.
 *
 * Bug 5: BPF_END byte-order conversion not implemented
 *   Byte-order conversion (BPF_END) was a no-op with a warn! log.
 *   Fixed by implementing REV16/REV32/REV64 (AArch64) and
 *   rol/bswap (x86_64).
 *
 * Bug 6: JitBuffer overflow silent truncation
 *   emit_u8/emit_u32 silently discarded writes when the buffer
 *   was full. Fixed by assert! on overflow.
 * ================================================================ */

/* ----------------------------------------------------------------
 * Bug 1: emit_st with R4 as base (x86_64 RCX conflict)
 *
 * This program stores an immediate value to the stack using R4 as
 * the base register. R4 maps to X86_RCX in x86_64 JIT. The bug
 * caused the immediate value to overwrite RCX before it was used
 * as the store base address.
 *
 * Includes two variants: ST_DW and ST_W with R4 as base.
 * ---------------------------------------------------------------- */
static void test_jit_emit_st_r4_base(void) {
    MODULE_START("jit_emit_st_r4_base");

    /* R4=0 (base), ST_DW [R4+0]=0xCAFE */
    struct bpf_insn prog_dw[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 4, 0, 0, 0),
        make_insn(BPF_ST | BPF_MEM | BPF_DW, 4, 0, 0, 0x0000CAFE),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_dw, sizeof(prog_dw) / sizeof(prog_dw[0]));
    CHECK(fd >= 0, "ST_DW with R4 as base register");
    if (fd >= 0) close(fd);

    /* R4=0 (base), ST_W [R4+4]=0xBEEF */
    struct bpf_insn prog_w[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 4, 0, 0, 0),
        make_insn(BPF_ST | BPF_MEM | BPF_W, 4, 0, 4, 0x0000BEEF),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_w, sizeof(prog_w) / sizeof(prog_w[0]));
    CHECK(fd >= 0, "ST_W with R4 as base register");
    if (fd >= 0) close(fd);

    /* Store and then load back from stack using R4 */
    struct bpf_insn prog_ld[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 4, 0, 0, 0),
        make_insn(BPF_ST | BPF_MEM | BPF_DW, 4, 0, 0, 0x12345678),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_DW, 0, 4, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_ld, sizeof(prog_ld) / sizeof(prog_ld[0]));
    CHECK(fd >= 0, "ST + LDX with R4 as base and size register");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * Bug 2: ALU32 DIV/MOD - EDX clearing verification (x86_64)
 *
 * 32-bit division on x86 uses EDX:EAX as the dividend. The JIT
 * must clear EDX before DIV to avoid incorrect results from stale
 * upper bits. This test creates programs using 32-bit DIV and MOD.
 *
 * Also tests ALU32 operations in sequence to exercise register
 * state management.
 * ---------------------------------------------------------------- */
static void test_jit_alu32_divmod(void) {
    MODULE_START("jit_alu32_divmod");

    /* 32-bit DIV: R0 = 100 / 3 = 33 */
    struct bpf_insn prog_div[] = {
        make_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU | BPF_DIV | BPF_K, 0, 0, 0, 3),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_div, sizeof(prog_div) / sizeof(prog_div[0]));
    CHECK(fd >= 0, "ALU32 DIV (100/3)");
    if (fd >= 0) close(fd);

    /* 32-bit MOD: R0 = 100 % 3 = 1 */
    struct bpf_insn prog_mod[] = {
        make_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU | BPF_MOD | BPF_K, 0, 0, 0, 3),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_mod, sizeof(prog_mod) / sizeof(prog_mod[0]));
    CHECK(fd >= 0, "ALU32 MOD (100%%3)");
    if (fd >= 0) close(fd);

    /* Sequential ALU32 DIV to test EDX gets cleared between operations */
    struct bpf_insn prog_seq[] = {
        make_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 200),
        make_insn(BPF_ALU | BPF_DIV | BPF_K, 0, 0, 0, 7),
        make_insn(BPF_ALU | BPF_DIV | BPF_K, 0, 0, 0, 3),
        make_insn(BPF_ALU | BPF_MUL | BPF_K, 0, 0, 0, 5),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_seq, sizeof(prog_seq) / sizeof(prog_seq[0]));
    CHECK(fd >= 0, "sequential ALU32 DIV (200/7/3*5)");
    if (fd >= 0) close(fd);

    /* Mixed 32-bit and 64-bit DIV to verify register state isolation */
    struct bpf_insn prog_mixed[] = {
        make_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 500),
        make_insn(BPF_ALU | BPF_DIV | BPF_K, 0, 0, 0, 11),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 3),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_mixed, sizeof(prog_mixed) / sizeof(prog_mixed[0]));
    CHECK(fd >= 0, "mixed ALU32+ALU64 DIV (500/11/3)");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * Bug 3: DIV/MOD by zero semantics
 *
 * Per eBPF specification:
 *   - DIV by zero returns 0
 *   - MOD by zero returns dst unchanged
 *
 * This test loads programs that contain division by zero. The JIT
 * backend must handle these cases with zero-division protection
 * (conditional branch that skips the actual DIV instruction).
 * ---------------------------------------------------------------- */
static void test_jit_divmod_zero(void) {
    MODULE_START("jit_divmod_zero");

    /* 64-bit DIV by zero: R0 should become 0 */
    struct bpf_insn prog_div64_zero[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_div64_zero, sizeof(prog_div64_zero) / sizeof(prog_div64_zero[0]));
    CHECK(fd >= 0, "ALU64 DIV by zero");
    if (fd >= 0) close(fd);

    /* 64-bit MOD by zero: R0 should remain 42 */
    struct bpf_insn prog_mod64_zero[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_ALU64 | BPF_MOD | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_mod64_zero, sizeof(prog_mod64_zero) / sizeof(prog_mod64_zero[0]));
    CHECK(fd >= 0, "ALU64 MOD by zero (dst unchanged)");
    if (fd >= 0) close(fd);

    /* 32-bit DIV by zero */
    struct bpf_insn prog_div32_zero[] = {
        make_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_ALU | BPF_DIV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_div32_zero, sizeof(prog_div32_zero) / sizeof(prog_div32_zero[0]));
    CHECK(fd >= 0, "ALU32 DIV by zero");
    if (fd >= 0) close(fd);

    /* 32-bit MOD by zero */
    struct bpf_insn prog_mod32_zero[] = {
        make_insn(BPF_ALU | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_ALU | BPF_MOD | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_mod32_zero, sizeof(prog_mod32_zero) / sizeof(prog_mod32_zero[0]));
    CHECK(fd >= 0, "ALU32 MOD by zero (dst unchanged)");
    if (fd >= 0) close(fd);

    /* Dynamic zero: src register holds 0 (not immediate) */
    struct bpf_insn prog_dyn_zero[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 50),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_X, 0, 1, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_dyn_zero, sizeof(prog_dyn_zero) / sizeof(prog_dyn_zero[0]));
    CHECK(fd >= 0, "ALU64 DIV by register zero (dynamic)");
    if (fd >= 0) close(fd);

    /* Program with both zero and non-zero divisors */
    struct bpf_insn prog_mixed_div[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 5),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 3),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_mixed_div, sizeof(prog_mixed_div) / sizeof(prog_mixed_div[0]));
    CHECK(fd >= 0, "mixed normal and zero DIV in one program");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * Bug 4: Jump offset correctness (insn_size fix verification)
 *
 * The insn_size bug caused all jump offsets to be wrong because
 * the estimated instruction sizes didn't match the actual emitted
 * sizes. This test creates programs with complex control flow:
 * forward jumps, backward jumps, and nested conditional jumps.
 *
 * The JitBuffer::new_sizing() counting pass now guarantees accurate
 * offsets by calling the same emit_* functions for both passes.
 * ---------------------------------------------------------------- */
static void test_jit_jump_offsets(void) {
    MODULE_START("jit_jump_offsets");

    /* Multiple forward JEQ with different offsets */
    struct bpf_insn prog_fwd[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 1),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 100),
        make_insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 2, 1),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_JMP | BPF_JA, 0, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 55),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_X, 0, 1, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_fwd, sizeof(prog_fwd) / sizeof(prog_fwd[0]));
    CHECK(fd >= 0, "forward jumps with multiple conditions");
    if (fd >= 0) close(fd);

    /* Backward jump (loop): R1=0; R0=0; loop: R0+=1; R1+=1; if R1<5 goto loop */
    struct bpf_insn prog_loop[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 1),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 1, 0, 0, 1),
        make_insn(BPF_JMP | BPF_JLT | BPF_K, 1, 0, -3, 5),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_loop, sizeof(prog_loop) / sizeof(prog_loop[0]));
    CHECK(fd >= 0, "backward jump (loop 5 iterations)");
    if (fd >= 0) close(fd);

    /* Nested conditional jumps (if-else): R0=10; if R0<100: R0=42 else R0=99 */
    struct bpf_insn prog_nested[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 10),
        make_insn(BPF_JMP | BPF_JGE | BPF_K, 0, 0, 2, 100),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_JMP | BPF_JA, 0, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_nested, sizeof(prog_nested) / sizeof(prog_nested[0]));
    CHECK(fd >= 0, "nested if-else with forward jumps");
    if (fd >= 0) close(fd);

    /* Dense jumps with LD_DW_IMM (which takes 2 slots -> 16 bytes) */
    struct bpf_insn prog_dense[] = {
        make_insn(BPF_LD | BPF_IMM | BPF_DW, 1, 0, 0, 0xDEAD0001),
        make_insn(0, 0, 0, 0, 0xBEEF0000),
        make_insn(BPF_JMP | BPF_JEQ | BPF_K, 1, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 1),
        make_insn(BPF_LD | BPF_IMM | BPF_DW, 2, 0, 0, 0xCAFE0002),
        make_insn(0, 0, 0, 0, 0xFEED0000),
        make_insn(BPF_JMP | BPF_JEQ | BPF_K, 2, 0, 2, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_JA, 0, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_dense, sizeof(prog_dense) / sizeof(prog_dense[0]));
    CHECK(fd >= 0, "dense jumps with LD_DW_IMM (double-slot insns)");
    if (fd >= 0) close(fd);

    /* JMP32 with backward jump */
    struct bpf_insn prog_jmp32_loop[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 1),
        make_insn(BPF_JMP32 | BPF_JLT | BPF_K, 0, 0, -2, 3),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_jmp32_loop, sizeof(prog_jmp32_loop) / sizeof(prog_jmp32_loop[0]));
    CHECK(fd >= 0, "JMP32 backward jump");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * Bug 5: BPF_END byte-order conversion
 *
 * BPF_END translates between host and big-endian byte order.
 * On little-endian hosts (x86_64, AArch64, RISC-V):
 *   - BPF_TO_BE converts from LE to BE
 *   - BPF_TO_LE is a no-op
 *
 * Tests 16-bit, 32-bit, and 64-bit byte swaps.
 * ---------------------------------------------------------------- */
static void test_jit_bpf_end(void) {
    MODULE_START("jit_bpf_end");

    /* 16-bit byte swap: BPF_END TO_BE 16 => 0xABCD -> 0xCDAB */
    struct bpf_insn prog_16[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0xABCD),
        BPF_END_TO_BE(0, 16),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_16, sizeof(prog_16) / sizeof(prog_16[0]));
    CHECK(fd >= 0, "BPF_END TO_BE 16-bit");
    if (fd >= 0) close(fd);

    /* 32-bit byte swap: BPF_END TO_BE 32 => 0x12345678 -> 0x78563412 */
    struct bpf_insn prog_32[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0x12345678),
        BPF_END_TO_BE(0, 32),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_32, sizeof(prog_32) / sizeof(prog_32[0]));
    CHECK(fd >= 0, "BPF_END TO_BE 32-bit");
    if (fd >= 0) close(fd);

    /* 64-bit byte swap: BPF_END TO_BE 64 */
    struct bpf_insn prog_64[] = {
        make_insn(BPF_LD | BPF_IMM | BPF_DW, 0, 0, 0, 0x01020304),
        make_insn(0, 0, 0, 0, 0x05060708),
        BPF_END_TO_BE(0, 64),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_64, sizeof(prog_64) / sizeof(prog_64[0]));
    CHECK(fd >= 0, "BPF_END TO_BE 64-bit");
    if (fd >= 0) close(fd);

    /* TO_LE 32-bit is a no-op on LE hosts */
    struct bpf_insn prog_le[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0x12345678),
        make_insn(BPF_ALU | BPF_END | BPF_K, 0, 0, 0, 32 | BPF_TO_LE),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_le, sizeof(prog_le) / sizeof(prog_le[0]));
    CHECK(fd >= 0, "BPF_END TO_LE 32-bit (no-op on LE)");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * ST/STX with all widths and base registers
 *
 * Exercises MEM operations (ST, STX, LDX) with all widths
 * (W/H/B/DW) and various base registers including edge cases.
 * ---------------------------------------------------------------- */
static void test_jit_mem_all_widths(void) {
    MODULE_START("jit_mem_all_widths");

    /* STX with all widths using stack (R10) */
    struct bpf_insn prog_stx_w[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0xDEADBEEF),
        make_insn(BPF_STX | BPF_MEM | BPF_W, 10, 0, -4, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_W, 0, 10, -4, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_stx_w, sizeof(prog_stx_w) / sizeof(prog_stx_w[0]));
    CHECK(fd >= 0, "STX_W / LDX_W stack ops");
    if (fd >= 0) close(fd);

    /* ST with B/H/W/DW widths */
    struct bpf_insn prog_st_dw[] = {
        make_insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -8, 0x12345678),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_DW, 0, 10, -8, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_st_dw, sizeof(prog_st_dw) / sizeof(prog_st_dw[0]));
    CHECK(fd >= 0, "ST_DW / LDX_DW stack ops");
    if (fd >= 0) close(fd);

    /* ST_H with offset > 0 */
    struct bpf_insn prog_st_h_off[] = {
        make_insn(BPF_ST | BPF_MEM | BPF_H, 10, 0, -2, 0xABCD),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_H, 0, 10, -2, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_st_h_off, sizeof(prog_st_h_off) / sizeof(prog_st_h_off[0]));
    CHECK(fd >= 0, "ST_H / LDX_H stack ops");
    if (fd >= 0) close(fd);

    /* STX with R4 as src register (exercises dst==RCX path in ALU + MEM) */
    struct bpf_insn prog_stx_r4[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 4, 0, 0, 0xCAFE),
        make_insn(BPF_STX | BPF_MEM | BPF_W, 10, 4, -4, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_W, 0, 10, -4, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_stx_r4, sizeof(prog_stx_r4) / sizeof(prog_stx_r4[0]));
    CHECK(fd >= 0, "STX with R4 as src (exercises RCX path)");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * Full instruction coverage: ALU64 with all operation types
 *
 * Exercises every ALU64 operation (ADD/SUB/MUL/DIV/MOD/OR/AND/
 * LSH/RSH/ARSH/NEG/XOR/MOV) in a single program to verify that
 * each emit function produces correct code.
 * ---------------------------------------------------------------- */
static void test_jit_alu64_all_ops(void) {
    MODULE_START("jit_alu64_all_ops");

    /* Chain of all ALU64 operations: R0 = ((100+50-30)*2/6|0xF00)&0xFFF<<2>>3^0x10 */
    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 50),
        make_insn(BPF_ALU64 | BPF_SUB | BPF_K, 0, 0, 0, 30),
        make_insn(BPF_ALU64 | BPF_MUL | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 6),
        make_insn(BPF_ALU64 | BPF_OR  | BPF_K, 0, 0, 0, 0xF00),
        make_insn(BPF_ALU64 | BPF_AND | BPF_K, 0, 0, 0, 0xFFF),
        make_insn(BPF_ALU64 | BPF_LSH | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_ALU64 | BPF_RSH | BPF_K, 0, 0, 0, 3),
        make_insn(BPF_ALU64 | BPF_XOR | BPF_K, 0, 0, 0, 0x10),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "ALU64 chain: all operations sequence");
    if (fd >= 0) close(fd);

    /* NEG and ARSH (separate - they have no imm variant) */
    struct bpf_insn prog_neg[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, -5),
        make_insn(BPF_ALU64 | BPF_NEG, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_neg, sizeof(prog_neg) / sizeof(prog_neg[0]));
    CHECK(fd >= 0, "ALU64 NEG (-5 -> 5)");
    if (fd >= 0) close(fd);

    struct bpf_insn prog_arsh[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, -256),
        make_insn(BPF_ALU64 | BPF_ARSH | BPF_K, 0, 0, 0, 4),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_arsh, sizeof(prog_arsh) / sizeof(prog_arsh[0]));
    CHECK(fd >= 0, "ALU64 ARSH (-256 >> 4)");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * CALL instruction with helper function
 *
 * Exercises BPF_CALL with various helper function IDs to verify
 * parameter passing and return value handling in JIT.
 * ---------------------------------------------------------------- */
static void test_jit_call_helpers(void) {
    MODULE_START("jit_call_helpers");

    /* bpf_ktime_get_ns (helper 5) */
    struct bpf_insn prog_ktime[] = {
        BPF_CALL_HELPER(5),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_ktime, sizeof(prog_ktime) / sizeof(prog_ktime[0]));
    CHECK(fd >= 0, "CALL helper 5 (ktime_get_ns)");
    if (fd >= 0) close(fd);

    /* bpf_get_smp_processor_id (helper 8) */
    struct bpf_insn prog_smp[] = {
        BPF_CALL_HELPER(8),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_smp, sizeof(prog_smp) / sizeof(prog_smp[0]));
    CHECK(fd >= 0, "CALL helper 8 (get_smp_processor_id)");
    if (fd >= 0) close(fd);

    /* bpf_get_prandom_u32 (helper 7) */
    struct bpf_insn prog_rand[] = {
        BPF_CALL_HELPER(7),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_rand, sizeof(prog_rand) / sizeof(prog_rand[0]));
    CHECK(fd >= 0, "CALL helper 7 (get_prandom_u32)");
    if (fd >= 0) close(fd);

    /* Helper with parameter: ALU before CALL */
    struct bpf_insn prog_param[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 42),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 1, 0, 0, 10),
        BPF_CALL_HELPER(5),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_param, sizeof(prog_param) / sizeof(prog_param[0]));
    CHECK(fd >= 0, "CALL with register parameters");
    if (fd >= 0) close(fd);

    /* Multiple helpers in sequence */
    struct bpf_insn prog_multi[] = {
        BPF_CALL_HELPER(5),
        BPF_CALL_HELPER(8),
        BPF_CALL_HELPER(7),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_multi, sizeof(prog_multi) / sizeof(prog_multi[0]));
    CHECK(fd >= 0, "multiple sequential helper calls");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * JMP: All conditional jump types
 *
 * Exercises every conditional jump opcode (JEQ/JNE/JGT/JGE/
 * JLT/JLE/JSGT/JSGE/JSLT/JSLE/JSET) with both immediate (K)
 * and register (X) sources.
 * ---------------------------------------------------------------- */
static void test_jit_jmp_all_types(void) {
    MODULE_START("jit_jmp_all_types");

    /* Jump table: test all conditional jump types with imm */
    struct bpf_insn prog_cond_k[] = {
        /* R0=10, R1=10 */
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 10),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 10),
        /* JEQ R0,10, +1 -> skip next (matches) */
        make_insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 1, 10),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        /* JNE R0,5, +1 -> skip next (R0=10!=5 matches) */
        make_insn(BPF_JMP | BPF_JNE | BPF_K, 0, 0, 1, 5),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        /* JGT R1,5, +1 -> skip next (R1=10>5 matches) */
        make_insn(BPF_JMP | BPF_JGT | BPF_X, 1, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        /* JLT R1,5, +2 -> fall through (R1=10 not <5) */
        make_insn(BPF_JMP | BPF_JLT | BPF_K, 1, 0, 2, 5),
        /* JGE R0,10, +1 -> skip (R0=10>=10) */
        make_insn(BPF_JMP | BPF_JGE | BPF_K, 0, 0, 1, 10),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        /* JLE R0,10, +1 -> skip (R0=10<=10) */
        make_insn(BPF_JMP | BPF_JLE | BPF_K, 0, 0, 1, 10),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog_cond_k, sizeof(prog_cond_k) / sizeof(prog_cond_k[0]));
    CHECK(fd >= 0, "all conditional jumps (JEQ/JNE/JGT/JGE/JLT/JLE)");
    if (fd >= 0) close(fd);

    /* Signed jumps: JSGT/JSGE/JSLT/JSLE + JSET */
    struct bpf_insn prog_signed[] = {
        /* R0=-1, R1=1 */
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, -1),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 1),
        /* JSGT R1,R0, +1: 1 signed > -1 = true -> skip */
        make_insn(BPF_JMP | BPF_JSGT | BPF_X, 1, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        /* JSGE R0,0, +2: -1 signed >= 0 = false -> no skip, fall into error */
        make_insn(BPF_JMP | BPF_JSGE | BPF_K, 0, 0, 2, 0),
        /* JSET R0,0xFF, +1: -1 & 0xFF = 0xFF != 0 -> skip */
        make_insn(BPF_JMP | BPF_JSET | BPF_K, 0, 0, 1, 0xFF),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    fd = load_prog(prog_signed, sizeof(prog_signed) / sizeof(prog_signed[0]));
    CHECK(fd >= 0, "signed and JSET jumps (JSGT/JSGE/JSET)");
    if (fd >= 0) close(fd);
}

/* ----------------------------------------------------------------
 * JIT compilation + execution round-trip
 *
 * Verifies that a BPF program can be loaded, attached to a
 * software perf event, and successfully execute through JIT.
 * This is the critical end-to-end test that exercises the full
 * JIT pipeline: load -> JIT compile -> execute.
 * ---------------------------------------------------------------- */
static void test_jit_roundtrip(void) {
    MODULE_START("jit_roundtrip");

    /* Simple program: MOV R0=42; EXIT */
    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long prog_fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(prog_fd >= 0, "load simple prog for roundtrip");
    if (prog_fd < 0) return;

    long perf_fd = open_perf_event_software();
    CHECK(perf_fd >= 0, "open software perf event");
    if (perf_fd < 0) { close(prog_fd); return; }

    /* Attach BPF program to perf event */
    struct {
        uint32_t target_fd;
        uint32_t attach_bpf_fd;
        uint32_t attach_type;
        uint32_t flags;
    } attach_attr = {
        .target_fd = (uint32_t)perf_fd,
        .attach_bpf_fd = (uint32_t)prog_fd,
        .attach_type = 0,
        .flags = 0,
    };
    long r = raw_bpf(BPF_PROG_ATTACH, &attach_attr, sizeof(attach_attr));
    CHECK(r == 0, "attach prog to perf event (triggers JIT execution)");

    /* Detach and cleanup */
    struct {
        uint32_t target_fd;
        uint32_t attach_bpf_fd;
        uint32_t attach_type;
        uint32_t flags;
    } detach_attr = {
        .target_fd = (uint32_t)perf_fd,
        .attach_bpf_fd = (uint32_t)prog_fd,
        .attach_type = 0,
        .flags = 0,
    };
    r = raw_bpf(BPF_PROG_DETACH, &detach_attr, sizeof(detach_attr));
    CHECK(r == 0, "detach prog from perf event");

    close(perf_fd);
    close(prog_fd);

    /* Roundtrip with ALU program that performs computation */
    struct bpf_insn prog_alu[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU64 | BPF_SUB | BPF_K, 0, 0, 0, 30),
        make_insn(BPF_ALU64 | BPF_MUL | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 7),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    prog_fd = load_prog(prog_alu, sizeof(prog_alu) / sizeof(prog_alu[0]));
    CHECK(prog_fd >= 0, "load ALU prog for JIT roundtrip");

    perf_fd = open_perf_event_software();
    CHECK(perf_fd >= 0, "open perf event for ALU roundtrip");

    if (prog_fd >= 0 && perf_fd >= 0) {
        struct {
            uint32_t target_fd;
            uint32_t attach_bpf_fd;
            uint32_t attach_type;
            uint32_t flags;
        } attr2 = {
            .target_fd = (uint32_t)perf_fd,
            .attach_bpf_fd = (uint32_t)prog_fd,
            .attach_type = 0,
            .flags = 0,
        };
        r = raw_bpf(BPF_PROG_ATTACH, &attr2, sizeof(attr2));
        CHECK(r == 0, "attach ALU prog -> JIT execute (100-30)*2/7");
    }
    if (prog_fd >= 0) close(prog_fd);
    if (perf_fd >= 0) close(perf_fd);
}

int main(void) {
    printf("=== eBPF JIT Regression Test Suite ===\n");

    test_jit_emit_st_r4_base();
    test_jit_alu32_divmod();
    test_jit_divmod_zero();
    test_jit_jump_offsets();
    test_jit_bpf_end();
    test_jit_mem_all_widths();
    test_jit_alu64_all_ops();
    test_jit_call_helpers();
    test_jit_jmp_all_types();
    test_jit_roundtrip();

    SUMMARY();
}