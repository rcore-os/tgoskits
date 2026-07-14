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

struct bpf_map_next_key_attr {
    uint64_t map_fd;
    uint64_t key;
    uint64_t next_key;
};

struct bpf_insn {
    uint8_t code;
    uint8_t dst_src_reg;
    int16_t off;
    int32_t imm;
};

#define BPF_MAP_TYPE_HASH  1
#define BPF_MAP_TYPE_ARRAY 2

#define BPF_PROG_TYPE_KPROBE 2

#define BPF_MAP_CREATE       0
#define BPF_MAP_LOOKUP_ELEM  1
#define BPF_MAP_UPDATE_ELEM  2
#define BPF_MAP_DELETE_ELEM  3
#define BPF_MAP_GET_NEXT_KEY 4
#define BPF_PROG_LOAD        5
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

#define BPF_JSGT  0x60
#define BPF_JSGE  0x70
#define BPF_JLT   0xa0
#define BPF_JLE   0xb0
#define BPF_JSLT  0xc0
#define BPF_JSLE  0xd0

#define BPF_CALL_HELPER(id) make_insn(BPF_JMP | BPF_CALL, 0, 0, 0, (int32_t)(id))

static struct bpf_insn make_insn(uint8_t code, uint8_t dst, uint8_t src, int16_t off, int32_t imm) {
    struct bpf_insn i;
    i.code = code;
    i.dst_src_reg = (dst & 0xf) | ((src & 0xf) << 4);
    i.off = off;
    i.imm = imm;
    return i;
}

static long create_array_map(uint32_t max_entries, uint32_t value_size) {
    struct bpf_map_create_attr attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = 4,
        .value_size = value_size,
        .max_entries = max_entries,
        .map_flags = 0,
    };
    return raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
}

static long create_hash_map(uint32_t max_entries, uint32_t key_size, uint32_t value_size) {
    struct bpf_map_create_attr attr = {
        .map_type = BPF_MAP_TYPE_HASH,
        .key_size = key_size,
        .value_size = value_size,
        .max_entries = max_entries,
        .map_flags = 0,
    };
    return raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
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

static long map_update(long map_fd, uint32_t key, uint64_t value) {
    struct bpf_map_elem_attr upd = {
        .map_fd = (uint64_t)map_fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&value,
        .flags = BPF_ANY,
    };
    return raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
}

static long map_lookup(long map_fd, uint32_t key, uint64_t *value) {
    struct bpf_map_elem_attr lookup = {
        .map_fd = (uint64_t)map_fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)value,
        .flags = 0,
    };
    return raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
}

static void test_prog_conditional_jmp(void) {
    MODULE_START("prog_conditional_jmp");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 200),
        make_insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 1, 100),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_X, 0, 1, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load conditional jump program");
    if (fd >= 0) close(fd);
}

static void test_prog_stack_ops(void) {
    MODULE_START("prog_stack_ops");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0xAABBCCDD12345678ULL & 0xFFFFFFFF),
        make_insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -8, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_DW, 0, 10, -8, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load stack store/load program");
    if (fd >= 0) close(fd);
}

static void test_prog_stx_w(void) {
    MODULE_START("prog_stx_w");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 0xDEADBEEF),
        make_insn(BPF_STX | BPF_MEM | BPF_W, 10, 1, -4, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_LDX | BPF_MEM | BPF_W, 0, 10, -4, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load STX.W / LDX.W program");
    if (fd >= 0) close(fd);
}

static void test_prog_alu32_truncation(void) {
    MODULE_START("prog_alu32_truncation");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, -1),
        make_insn(BPF_ALU | BPF_AND | BPF_K, 0, 0, 0, 0xFF),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load ALU32 truncation program");
    if (fd >= 0) close(fd);
}

static void test_prog_jmp32(void) {
    MODULE_START("prog_jmp32");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 42),
        make_insn(BPF_JMP32 | BPF_JEQ | BPF_X, 0, 1, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load JMP32 program");
    if (fd >= 0) close(fd);
}

static void test_prog_ld_dw_imm(void) {
    MODULE_START("prog_ld_dw_imm");

    struct bpf_insn prog[] = {
        make_insn(BPF_LD | BPF_IMM | BPF_DW, 0, 0, 0, 0x12345678),
        make_insn(0, 0, 0, 0, 0x9ABCDEF0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load LD_DW_IMM program (64-bit immediate)");
    if (fd >= 0) close(fd);
}

static void test_prog_alu_ops(void) {
    MODULE_START("prog_alu_ops");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU64 | BPF_SUB | BPF_K, 0, 0, 0, 30),
        make_insn(BPF_ALU64 | BPF_MUL | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_ALU64 | BPF_DIV | BPF_K, 0, 0, 0, 7),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 10),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load ALU ops program (100-30)*2/7+10");
    if (fd >= 0) close(fd);
}

static void test_prog_alu_xor_or_and(void) {
    MODULE_START("prog_alu_xor_or_and");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0xFF00),
        make_insn(BPF_ALU64 | BPF_AND | BPF_K, 0, 0, 0, 0xF0F0),
        make_insn(BPF_ALU64 | BPF_OR  | BPF_K, 0, 0, 0, 0x000F),
        make_insn(BPF_ALU64 | BPF_XOR | BPF_K, 0, 0, 0, 0x0050),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load XOR/OR/AND program");
    if (fd >= 0) close(fd);
}

static void test_prog_alu_shift(void) {
    MODULE_START("prog_alu_shift");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 1),
        make_insn(BPF_ALU64 | BPF_LSH | BPF_K, 0, 0, 0, 20),
        make_insn(BPF_ALU64 | BPF_RSH | BPF_K, 0, 0, 0, 10),
        make_insn(BPF_ALU64 | BPF_ARSH | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load shift program (LSH/RSH/ARSH)");
    if (fd >= 0) close(fd);
}

static void test_prog_neg_mod(void) {
    MODULE_START("prog_neg_mod");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 100),
        make_insn(BPF_ALU64 | BPF_MOD | BPF_K, 0, 0, 0, 7),
        make_insn(BPF_ALU64 | BPF_NEG, 0, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 2),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load NEG/MOD program");
    if (fd >= 0) close(fd);
}

static void test_prog_jmp_variants(void) {
    MODULE_START("prog_jmp_variants");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 5),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, 10),
        make_insn(BPF_JMP | BPF_JGT | BPF_X, 1, 0, 1, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_JNE | BPF_K, 0, 0, 1, 5),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 99),
        make_insn(BPF_JMP | BPF_JGE | BPF_K, 0, 0, 1, 99),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load JMP variants (JGT/JNE/JGE) program");
    if (fd >= 0) close(fd);
}

static void test_prog_call_map_lookup(void) {
    MODULE_START("prog_call_map_lookup");

    long map_fd = create_array_map(4, 8);
    CHECK(map_fd >= 0, "create array map for helper test");
    if (map_fd < 0) return;

    uint32_t key = 0;
    uint64_t val = 0x4242424242424242ULL;
    long r = map_update(map_fd, key, val);
    CHECK(r == 0, "update array[0] = 0x4242...");
    (void)r;

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 1, 0, 0, (int32_t)map_fd),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 2, 0, 0, 0),
        make_insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 2, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, (int32_t)((uintptr_t)&key)),
        BPF_CALL_HELPER(1),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };

    (void)prog;
    printf("  INFO | map_lookup helper program constructed (map_fd=%ld)\n", map_fd);

    close(map_fd);
}

static void test_prog_call_ktime(void) {
    MODULE_START("prog_call_ktime");

    struct bpf_insn prog[] = {
        BPF_CALL_HELPER(5),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load bpf_ktime_get_ns helper program");
    if (fd >= 0) close(fd);
}

static void test_prog_call_get_pid_tgid(void) {
    MODULE_START("prog_call_get_pid_tgid");

    struct bpf_insn prog[] = {
        BPF_CALL_HELPER(14),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load bpf_get_current_pid_tgid helper program");
    if (fd >= 0) close(fd);
}

static void test_prog_call_get_uid_gid(void) {
    MODULE_START("prog_call_get_uid_gid");

    struct bpf_insn prog[] = {
        BPF_CALL_HELPER(15),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load bpf_get_current_uid_gid helper program");
    if (fd >= 0) close(fd);
}

static void test_prog_call_get_smp_id(void) {
    MODULE_START("prog_call_get_smp_id");

    struct bpf_insn prog[] = {
        BPF_CALL_HELPER(8),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load bpf_get_smp_processor_id helper program");
    if (fd >= 0) close(fd);
}

static void test_prog_call_prandom(void) {
    MODULE_START("prog_call_prandom");

    struct bpf_insn prog[] = {
        BPF_CALL_HELPER(7),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load bpf_get_prandom_u32 helper program");
    if (fd >= 0) close(fd);
}

static void test_map_obj_close(void) {
    MODULE_START("map_obj_close");

    long fd = create_array_map(4, 8);
    CHECK(fd >= 0, "create array map for close test");
    if (fd < 0) return;

    uint32_t close_fd = (uint32_t)fd;
    long r = raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    CHECK(r == 0, "BPF_OBJ_CLOSE map fd succeeds");

    uint32_t key = 0;
    uint64_t val = 0;
    struct bpf_map_elem_attr lookup = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&val,
        .flags = 0,
    };
    r = raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
    CHECK(r < 0, "lookup on closed map fd returns error");
}

static void test_prog_obj_close(void) {
    MODULE_START("prog_obj_close");

    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load program for close test");
    if (fd < 0) return;

    uint32_t close_fd = (uint32_t)fd;
    long r = raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    CHECK(r == 0, "BPF_OBJ_CLOSE prog fd succeeds");
}

static void test_obj_close_invalid(void) {
    MODULE_START("obj_close_invalid");

    uint32_t bad_fd = 9999;
    long r = raw_bpf(BPF_OBJ_CLOSE, &bad_fd, sizeof(bad_fd));
    CHECK(r < 0, "BPF_OBJ_CLOSE on invalid fd returns error");
}

static void test_map_stress_array(void) {
    MODULE_START("map_stress_array");

    long fd = create_array_map(256, 8);
    CHECK(fd >= 0, "create 256-entry array map");
    if (fd < 0) return;

    int ok = 1;
    for (uint32_t i = 0; i < 256; i++) {
        uint64_t val = (uint64_t)i * 0x0101010101010101ULL;
        if (map_update(fd, i, val) != 0) { ok = 0; break; }
    }
    CHECK(ok, "update all 256 entries");

    ok = 1;
    for (uint32_t i = 0; i < 256; i++) {
        uint64_t got = 0;
        if (map_lookup(fd, i, &got) != 0 || got != (uint64_t)i * 0x0101010101010101ULL) {
            ok = 0; break;
        }
    }
    CHECK(ok, "lookup all 256 entries, verify values");

    close(fd);
}

static void test_map_stress_hash(void) {
    MODULE_START("map_stress_hash");

    long fd = create_hash_map(128, 4, 8);
    CHECK(fd >= 0, "create 128-entry hash map");
    if (fd < 0) return;

    int ok = 1;
    for (uint32_t i = 0; i < 100; i++) {
        uint64_t val = (uint64_t)i + 1000;
        if (map_update(fd, i, val) != 0) { ok = 0; break; }
    }
    CHECK(ok, "update 100 entries in hash map");

    ok = 1;
    for (uint32_t i = 0; i < 100; i++) {
        uint64_t got = 0;
        if (map_lookup(fd, i, &got) != 0 || got != (uint64_t)i + 1000) {
            ok = 0; break;
        }
    }
    CHECK(ok, "lookup 100 entries in hash map, verify values");

    ok = 1;
    for (uint32_t i = 0; i < 100; i++) {
        struct bpf_map_elem_attr del = {
            .map_fd = (uint64_t)fd,
            .key = (uint64_t)&(uint32_t){i},
            .value = 0,
            .flags = 0,
        };
        if (raw_bpf(BPF_MAP_DELETE_ELEM, &del, sizeof(del)) != 0) { ok = 0; break; }
    }
    CHECK(ok, "delete all 100 entries");

    close(fd);
}

static void test_map_update_flags(void) {
    MODULE_START("map_update_flags");

    long fd = create_hash_map(4, 4, 8);
    CHECK(fd >= 0, "create hash map for flags test");
    if (fd < 0) return;

    uint32_t key = 1;
    uint64_t val = 100;
    struct bpf_map_elem_attr upd = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&val,
        .flags = BPF_ANY,
    };
    long r = raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    CHECK(r == 0, "BPF_ANY update on empty key succeeds");

    val = 200;
    upd.flags = 2;
    r = raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    CHECK(r == 0, "BPF_EXISTS update on existing key succeeds");

    uint32_t key2 = 2;
    uint64_t val2 = 300;
    upd.key = (uint64_t)&key2;
    upd.value = (uint64_t)&val2;
    upd.flags = 2;
    r = raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    CHECK(r < 0, "BPF_EXISTS update on non-existing key fails");

    close(fd);
}

static void test_map_fd_reuse(void) {
    MODULE_START("map_fd_reuse");

    long fd1 = create_array_map(4, 8);
    CHECK(fd1 >= 0, "create first array map");
    if (fd1 < 0) return;

    long fd2 = create_array_map(4, 8);
    CHECK(fd2 >= 0, "create second array map");
    CHECK(fd2 != fd1, "second map gets different fd");

    if (fd2 >= 0) {
        uint32_t close_fd = (uint32_t)fd1;
        raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    }

    long fd3 = create_array_map(4, 8);
    CHECK(fd3 >= 0, "create third array map after closing first");
    if (fd3 >= 0) {
        close(fd3);
    }
    if (fd2 >= 0) close(fd2);
}

static void test_prog_multijmp(void) {
    MODULE_START("prog_multijmp");

    struct bpf_insn prog[] = {
        make_insn(BPF_JMP | BPF_JA, 0, 0, 2, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    long fd = load_prog(prog, sizeof(prog) / sizeof(prog[0]));
    CHECK(fd >= 0, "load unconditional jump (JA) program");
    if (fd >= 0) close(fd);
}

static void test_map_hash_get_next_key(void) {
    MODULE_START("map_hash_get_next_key");

    long fd = create_hash_map(16, 4, 4);
    CHECK(fd >= 0, "create hash map for get_next_key");
    if (fd < 0) return;

    uint32_t keys[] = {10, 20, 30, 40, 50};
    uint32_t vals[] = {100, 200, 300, 400, 500};
    for (int i = 0; i < 5; i++) {
        map_update(fd, keys[i], (uint64_t)vals[i]);
    }

    uint32_t first_key = 0;
    struct bpf_map_next_key_attr nk = {
        .map_fd = (uint64_t)fd,
        .key = 0,
        .next_key = (uint64_t)&first_key,
    };
    long r = raw_bpf(BPF_MAP_GET_NEXT_KEY, &nk, sizeof(nk));
    CHECK(r == 0, "get_first_key (NULL key) succeeds");

    int count = (r == 0) ? 1 : 0;
    uint32_t cur = first_key;
    for (int iter = 0; iter < 10; iter++) {
        nk.key = (uint64_t)&cur;
        nk.next_key = (uint64_t)&first_key;
        r = raw_bpf(BPF_MAP_GET_NEXT_KEY, &nk, sizeof(nk));
        if (r != 0) break;
        count++;
        cur = first_key;
    }
    CHECK(count == 5, "iterating 5 keys yields correct count");

    close(fd);
}

int main(void) {
    printf("=== eBPF Advanced Test Suite ===\n");

    test_prog_conditional_jmp();
    test_prog_stack_ops();
    test_prog_stx_w();
    test_prog_alu32_truncation();
    test_prog_jmp32();
    test_prog_ld_dw_imm();
    test_prog_alu_ops();
    test_prog_alu_xor_or_and();
    test_prog_alu_shift();
    test_prog_neg_mod();
    test_prog_jmp_variants();
    test_prog_multijmp();
    test_prog_call_ktime();
    test_prog_call_get_pid_tgid();
    test_prog_call_get_uid_gid();
    test_prog_call_get_smp_id();
    test_prog_call_prandom();
    test_prog_call_map_lookup();
    test_map_obj_close();
    test_prog_obj_close();
    test_obj_close_invalid();
    test_map_stress_array();
    test_map_stress_hash();
    test_map_update_flags();
    test_map_fd_reuse();
    test_map_hash_get_next_key();

    SUMMARY();
}
