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

struct bpf_map_next_key_attr {
    uint64_t map_fd;
    uint64_t key;
    uint64_t next_key;
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

#define BPF_MAP_TYPE_HASH  1
#define BPF_MAP_TYPE_ARRAY 2

#define BPF_PROG_TYPE_KPROBE 2
#define BPF_PROG_TYPE_RAW_TRACEPOINT 17

#define BPF_MAP_CREATE       0
#define BPF_MAP_LOOKUP_ELEM  1
#define BPF_MAP_UPDATE_ELEM  2
#define BPF_MAP_DELETE_ELEM  3
#define BPF_MAP_GET_NEXT_KEY 4
#define BPF_PROG_LOAD        5
#define BPF_PROG_ATTACH      8
#define BPF_PROG_DETACH      9
#define BPF_RAW_TRACEPOINT_OPEN 17

#define BPF_ANY   0
#define BPF_EXISTS 2

#define BPF_ALU64   0x07
#define BPF_MOV     0xb0
#define BPF_K       0x00
#define BPF_JMP     0x05
#define BPF_JA      0x00
#define BPF_EXIT    0x90
#define BPF_ST      0x02
#define BPF_MEM     0x60
#define BPF_DW      0x18
#define BPF_JEQ     0x10
#define BPF_X       0x08
#define BPF_ADD     0x00
#define BPF_ALU     0x04
#define BPF_STX     0x03
#define BPF_W       0x00
#define BPF_LD      0x00
#define BPF_IMM     0x00

static int ebpf_available(void) {
    struct bpf_map_create_attr attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = 4,
        .value_size = 8,
        .max_entries = 1,
        .map_flags = 0,
    };
    errno = 0;
    long fd = raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    if (fd >= 0) {
        close(fd);
        return 1;
    }
    if (errno == ENOSYS) {
        printf("eBPF unavailable: bpf(2) returned ENOSYS; skipping positive eBPF tests\n");
        return 0;
    }
    return 1;
}

static struct bpf_insn make_insn(uint8_t code, uint8_t dst, uint8_t src, int16_t off, int32_t imm) {
    struct bpf_insn i;
    i.code = code;
    i.dst_src_reg = (dst & 0xf) | ((src & 0xf) << 4);
    i.off = off;
    i.imm = imm;
    return i;
}

static void test_map_create_array(void) {
    MODULE_START("map_create_array");
    struct bpf_map_create_attr attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = 4,
        .value_size = 8,
        .max_entries = 16,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    CHECK(fd >= 0, "BPF_MAP_CREATE array returns valid fd");
    if (fd >= 0) close(fd);
}

static void test_map_create_hash(void) {
    MODULE_START("map_create_hash");
    struct bpf_map_create_attr attr = {
        .map_type = BPF_MAP_TYPE_HASH,
        .key_size = 4,
        .value_size = 8,
        .max_entries = 64,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    CHECK(fd >= 0, "BPF_MAP_CREATE hash returns valid fd");
    if (fd >= 0) close(fd);
}

static void test_map_update_lookup_array(void) {
    MODULE_START("map_update_lookup_array");
    struct bpf_map_create_attr create = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = 4,
        .value_size = 8,
        .max_entries = 16,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &create, sizeof(create));
    CHECK(fd >= 0, "create array map");
    if (fd < 0) return;

    uint32_t key = 3;
    uint64_t value = 0xDEADBEEFCAFE1234ULL;
    struct bpf_map_elem_attr upd = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&value,
        .flags = BPF_ANY,
    };
    long r = raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    CHECK(r == 0, "update array[3] = 0xDEADBEEF...");

    uint64_t got = 0;
    struct bpf_map_elem_attr lookup = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&got,
        .flags = 0,
    };
    r = raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
    CHECK(r == 0, "lookup array[3] succeeds");
    CHECK(got == 0xDEADBEEFCAFE1234ULL, "lookup returns correct value");

    key = 3;
    value = 42;
    upd.value = (uint64_t)&value;
    raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));

    got = 0;
    raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
    CHECK(got == 42, "update-then-lookup returns 42");

    close(fd);
}

static void test_map_update_lookup_hash(void) {
    MODULE_START("map_update_lookup_hash");
    struct bpf_map_create_attr create = {
        .map_type = BPF_MAP_TYPE_HASH,
        .key_size = 4,
        .value_size = 8,
        .max_entries = 64,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &create, sizeof(create));
    CHECK(fd >= 0, "create hash map");
    if (fd < 0) return;

    uint32_t key = 100;
    uint64_t value = 9999;
    struct bpf_map_elem_attr upd = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&value,
        .flags = BPF_ANY,
    };
    long r = raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    CHECK(r == 0, "hash update key=100 val=9999");

    uint64_t got = 0;
    struct bpf_map_elem_attr lookup = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = (uint64_t)&got,
        .flags = 0,
    };
    r = raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
    CHECK(r == 0, "hash lookup key=100 succeeds");
    CHECK(got == 9999, "hash lookup returns 9999");

    struct bpf_map_elem_attr del = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&key,
        .value = 0,
        .flags = 0,
    };
    r = raw_bpf(BPF_MAP_DELETE_ELEM, &del, sizeof(del));
    CHECK(r == 0, "hash delete key=100 succeeds");

    got = 0;
    r = raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
    CHECK(r < 0, "hash lookup after delete fails");

    close(fd);
}

static void test_map_get_next_key(void) {
    MODULE_START("map_get_next_key");
    struct bpf_map_create_attr create = {
        .map_type = BPF_MAP_TYPE_HASH,
        .key_size = 4,
        .value_size = 4,
        .max_entries = 64,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &create, sizeof(create));
    CHECK(fd >= 0, "create hash map for iteration");
    if (fd < 0) return;

    for (uint32_t i = 10; i <= 30; i += 10) {
        uint32_t val = i * 100;
        struct bpf_map_elem_attr upd = {
            .map_fd = (uint64_t)fd,
            .key = (uint64_t)&i,
            .value = (uint64_t)&val,
            .flags = BPF_ANY,
        };
        raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    }

    uint32_t first_key = 0;
    struct bpf_map_next_key_attr nk = {
        .map_fd = (uint64_t)fd,
        .key = 0,
        .next_key = (uint64_t)&first_key,
    };
    long r = raw_bpf(BPF_MAP_GET_NEXT_KEY, &nk, sizeof(nk));
    CHECK(r == 0, "get_first_key (NULL key) succeeds");
    int count = 0;
    if (r == 0) count = 1;

    uint32_t cur = first_key;
    for (int iter = 0; iter < 10; iter++) {
        nk.key = (uint64_t)&cur;
        nk.next_key = (uint64_t)&first_key;
        r = raw_bpf(BPF_MAP_GET_NEXT_KEY, &nk, sizeof(nk));
        if (r != 0) break;
        count++;
        cur = first_key;
    }
    CHECK(count == 3, "iterating hash map yields 3 keys");

    close(fd);
}

static void test_prog_load(void) {
    MODULE_START("prog_load");
    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 42),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    struct bpf_prog_load_attr attr = {
        .prog_type = BPF_PROG_TYPE_KPROBE,
        .insn_cnt = sizeof(prog) / sizeof(prog[0]),
        .insns = (uint64_t)prog,
        .license = 0,
        .log_level = 0,
        .log_size = 0,
        .log_buf = 0,
        .kern_version = 0,
        .prog_flags = 0,
    };
    long fd = raw_bpf(BPF_PROG_LOAD, &attr, sizeof(attr));
    CHECK(fd >= 0, "BPF_PROG_LOAD simple return-42 program");
    if (fd >= 0) close(fd);
}

static void test_prog_load_with_alu(void) {
    MODULE_START("prog_load_with_alu");
    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 10),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 20),
        make_insn(BPF_ALU64 | BPF_ADD | BPF_K, 0, 0, 0, 12),
        make_insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    struct bpf_prog_load_attr attr = {
        .prog_type = BPF_PROG_TYPE_KPROBE,
        .insn_cnt = sizeof(prog) / sizeof(prog[0]),
        .insns = (uint64_t)prog,
        .license = 0,
        .log_level = 0,
        .log_size = 0,
        .log_buf = 0,
        .kern_version = 0,
        .prog_flags = 0,
    };
    long fd = raw_bpf(BPF_PROG_LOAD, &attr, sizeof(attr));
    CHECK(fd >= 0, "BPF_PROG_LOAD alu program (10+20+12)");
    if (fd >= 0) close(fd);
}

static void test_map_create_invalid(void) {
    MODULE_START("map_create_invalid");
    struct bpf_map_create_attr attr = {
        .map_type = 99,
        .key_size = 4,
        .value_size = 4,
        .max_entries = 10,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    CHECK(fd < 0, "BPF_MAP_CREATE with invalid type returns error");

    attr.map_type = BPF_MAP_TYPE_HASH;
    attr.key_size = 0;
    fd = raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    CHECK(fd < 0, "BPF_MAP_CREATE with key_size=0 returns error");
}

static void test_map_operations_invalid(void) {
    MODULE_START("map_operations_invalid");
    uint32_t key = 0;
    uint64_t val = 0;
    struct bpf_map_elem_attr lookup = {
        .map_fd = 9999,
        .key = (uint64_t)&key,
        .value = (uint64_t)&val,
        .flags = 0,
    };
    long r = raw_bpf(BPF_MAP_LOOKUP_ELEM, &lookup, sizeof(lookup));
    CHECK(r < 0, "lookup on invalid fd returns error");

    struct bpf_map_elem_attr upd = {
        .map_fd = 9999,
        .key = (uint64_t)&key,
        .value = (uint64_t)&val,
        .flags = 0,
    };
    r = raw_bpf(BPF_MAP_UPDATE_ELEM, &upd, sizeof(upd));
    CHECK(r < 0, "update on invalid fd returns error");
}

int main(void) {
    printf("=== eBPF Basics Test Suite ===\n");

    if (!ebpf_available()) {
        return 0;
    }

    test_map_create_array();
    test_map_create_hash();
    test_map_update_lookup_array();
    test_map_update_lookup_hash();
    test_map_get_next_key();
    test_prog_load();
    test_prog_load_with_alu();
    test_map_create_invalid();
    test_map_operations_invalid();

    SUMMARY();
}
