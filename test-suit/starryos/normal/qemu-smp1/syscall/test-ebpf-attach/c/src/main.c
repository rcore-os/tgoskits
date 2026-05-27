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

static long raw_perf_event_open(void *attr, int32_t pid, int32_t cpu,
                                 int32_t group_fd, uint64_t flags) {
#ifdef SYS_perf_event_open
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
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

#define BPF_PROG_TYPE_KPROBE 2

#define BPF_MAP_CREATE       0
#define BPF_PROG_LOAD        5
#define BPF_PROG_ATTACH      8
#define BPF_PROG_DETACH      9
#define BPF_OBJ_CLOSE       11
#define BPF_LINK_CREATE     28

#define BPF_ANY   0

#define BPF_ALU64 0x07
#define BPF_JMP   0x05
#define BPF_MOV   0xb0
#define BPF_K     0x00
#define BPF_EXIT  0x90
#define BPF_CALL  0x80

#define PERF_TYPE_SOFTWARE   1
#define PERF_TYPE_TRACEPOINT 2
#define PERF_TYPE_KPROBE     6

#define PERF_COUNT_SW_CPU_CLOCK  0

static struct bpf_insn make_insn(uint8_t code, uint8_t dst, uint8_t src, int16_t off, int32_t imm) {
    struct bpf_insn i;
    i.code = code;
    i.dst_src_reg = (dst & 0xf) | ((src & 0xf) << 4);
    i.off = off;
    i.imm = imm;
    return i;
}

static long load_simple_prog(void) {
    struct bpf_insn prog[] = {
        make_insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
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
    return raw_bpf(BPF_PROG_LOAD, &attr, sizeof(attr));
}

static long open_perf_event_software(void) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_SOFTWARE;
    attr.size = sizeof(attr);
    attr.config = PERF_COUNT_SW_CPU_CLOCK;
    return raw_perf_event_open(&attr, -1, 0, -1, 0);
}

static long open_perf_event_kprobe(void) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_KPROBE;
    attr.size = sizeof(attr);
    attr.config = 0;
    return raw_perf_event_open(&attr, -1, 0, -1, 0);
}

static void test_perf_event_open_software(void) {
    MODULE_START("perf_event_open_software");
    long fd = open_perf_event_software();
    CHECK(fd >= 0, "perf_event_open(SOFTWARE, CPU_CLOCK) returns valid fd");
    if (fd >= 0) close(fd);
}

static void test_perf_event_open_kprobe(void) {
    MODULE_START("perf_event_open_kprobe");
    long fd = open_perf_event_kprobe();
    CHECK(fd >= 0, "perf_event_open(KPROBE) returns valid fd");
    if (fd >= 0) close(fd);
}

static void test_perf_event_open_null_attr(void) {
    MODULE_START("perf_event_open_null_attr");
    long fd = raw_perf_event_open(NULL, -1, 0, -1, 0);
    CHECK(fd < 0, "perf_event_open(NULL attr) returns error");
}

static void test_perf_event_open_invalid_type(void) {
    MODULE_START("perf_event_open_invalid_type");
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = 99;
    attr.size = sizeof(attr);
    attr.config = 0;
    long fd = raw_perf_event_open(&attr, -1, 0, -1, 0);
    CHECK(fd < 0, "perf_event_open(type=99) returns error");
}

static void test_prog_attach_basic(void) {
    MODULE_START("prog_attach_basic");

    long prog_fd = load_simple_prog();
    CHECK(prog_fd >= 0, "load BPF program for attach test");
    if (prog_fd < 0) return;

    long perf_fd = open_perf_event_software();
    CHECK(perf_fd >= 0, "open perf event for attach test");
    if (perf_fd < 0) { close(prog_fd); return; }

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
    CHECK(r == 0, "BPF_PROG_ATTACH succeeds");

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
    CHECK(r == 0, "BPF_PROG_DETACH succeeds");

    close(perf_fd);
    close(prog_fd);
}

static void test_prog_attach_invalid_fd(void) {
    MODULE_START("prog_attach_invalid_fd");

    struct {
        uint32_t target_fd;
        uint32_t attach_bpf_fd;
        uint32_t attach_type;
        uint32_t flags;
    } attr = {
        .target_fd = 9999,
        .attach_bpf_fd = 9998,
        .attach_type = 0,
        .flags = 0,
    };
    long r = raw_bpf(BPF_PROG_ATTACH, &attr, sizeof(attr));
    CHECK(r < 0, "BPF_PROG_ATTACH with invalid fds returns error");
}

static void test_link_create_basic(void) {
    MODULE_START("link_create_basic");

    long prog_fd = load_simple_prog();
    CHECK(prog_fd >= 0, "load BPF program for link_create test");
    if (prog_fd < 0) return;

    long perf_fd = open_perf_event_kprobe();
    CHECK(perf_fd >= 0, "open perf event (kprobe) for link_create test");
    if (perf_fd < 0) { close(prog_fd); return; }

    struct {
        uint32_t prog_fd;
        uint32_t target_fd;
        uint32_t attach_type;
        uint32_t flags;
    } link_attr = {
        .prog_fd = (uint32_t)prog_fd,
        .target_fd = (uint32_t)perf_fd,
        .attach_type = 0,
        .flags = 0,
    };
    long link_fd = raw_bpf(BPF_LINK_CREATE, &link_attr, sizeof(link_attr));
    CHECK(link_fd >= 0, "BPF_LINK_CREATE returns valid link fd");
    CHECK(link_fd != prog_fd, "link fd differs from prog fd");
    CHECK(link_fd != perf_fd, "link fd differs from perf fd");

    if (link_fd >= 0) {
        uint32_t close_fd = (uint32_t)link_fd;
        raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    }

    close(perf_fd);
    close(prog_fd);
}

static void test_link_create_invalid_fd(void) {
    MODULE_START("link_create_invalid_fd");

    struct {
        uint32_t prog_fd;
        uint32_t target_fd;
        uint32_t attach_type;
        uint32_t flags;
    } link_attr = {
        .prog_fd = 9999,
        .target_fd = 9998,
        .attach_type = 0,
        .flags = 0,
    };
    long r = raw_bpf(BPF_LINK_CREATE, &link_attr, sizeof(link_attr));
    CHECK(r < 0, "BPF_LINK_CREATE with invalid fds returns error");
}

static void test_obj_close_map(void) {
    MODULE_START("obj_close_map");

    struct bpf_map_create_attr attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = 4,
        .value_size = 8,
        .max_entries = 4,
        .map_flags = 0,
    };
    long fd = raw_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    CHECK(fd >= 0, "create array map for obj_close test");
    if (fd < 0) return;

    uint32_t close_fd = (uint32_t)fd;
    long r = raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    CHECK(r == 0, "BPF_OBJ_CLOSE map fd succeeds");

    struct bpf_map_elem_attr lookup = {
        .map_fd = (uint64_t)fd,
        .key = (uint64_t)&(uint32_t){0},
        .value = (uint64_t)&(uint64_t){0},
        .flags = 0,
    };
    r = raw_bpf(1, &lookup, sizeof(lookup));
    CHECK(r < 0, "lookup on closed map fd fails");
}

static void test_obj_close_prog(void) {
    MODULE_START("obj_close_prog");

    long fd = load_simple_prog();
    CHECK(fd >= 0, "load prog for obj_close test");
    if (fd < 0) return;

    uint32_t close_fd = (uint32_t)fd;
    long r = raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    CHECK(r == 0, "BPF_OBJ_CLOSE prog fd succeeds");
}

static void test_obj_close_invalid(void) {
    MODULE_START("obj_close_invalid");

    uint32_t bad_fd = 9999;
    long r = raw_bpf(BPF_OBJ_CLOSE, &bad_fd, sizeof(bad_fd));
    CHECK(r < 0, "BPF_OBJ_CLOSE with non-existent fd returns error");

    uint32_t zero_fd = 0;
    r = raw_bpf(BPF_OBJ_CLOSE, &zero_fd, sizeof(zero_fd));
    CHECK(r < 0, "BPF_OBJ_CLOSE with fd=0 returns error");
}

static void test_full_lifecycle(void) {
    MODULE_START("full_lifecycle");

    long prog_fd = load_simple_prog();
    CHECK(prog_fd >= 0, "step1: load BPF program");
    if (prog_fd < 0) return;

    long perf_fd = open_perf_event_software();
    CHECK(perf_fd >= 0, "step2: open perf event");
    if (perf_fd < 0) { close(prog_fd); return; }

    struct {
        uint32_t prog_fd;
        uint32_t target_fd;
        uint32_t attach_type;
        uint32_t flags;
    } link_attr = {
        .prog_fd = (uint32_t)prog_fd,
        .target_fd = (uint32_t)perf_fd,
        .attach_type = 0,
        .flags = 0,
    };
    long link_fd = raw_bpf(BPF_LINK_CREATE, &link_attr, sizeof(link_attr));
    CHECK(link_fd >= 0, "step3: BPF_LINK_CREATE");
    if (link_fd < 0) { close(perf_fd); close(prog_fd); return; }

    uint32_t close_link = (uint32_t)link_fd;
    long r = raw_bpf(BPF_OBJ_CLOSE, &close_link, sizeof(close_link));
    CHECK(r == 0, "step4: close link fd");

    uint32_t close_prog = (uint32_t)prog_fd;
    r = raw_bpf(BPF_OBJ_CLOSE, &close_prog, sizeof(close_prog));
    CHECK(r == 0, "step5: close prog fd");

    close(perf_fd);
}

static void test_multiple_prog_load_close(void) {
    MODULE_START("multiple_prog_load_close");

    long fds[8];
    int loaded = 0;
    for (int i = 0; i < 8; i++) {
        fds[i] = load_simple_prog();
        if (fds[i] < 0) break;
        loaded++;
    }
    CHECK(loaded == 8, "load 8 BPF programs");
    for (int i = 0; i < loaded; i++) {
        CHECK(fds[i] > 0, "each prog gets valid fd");
    }
    for (int i = 0; i < loaded; i++) {
        uint32_t close_fd = (uint32_t)fds[i];
        raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    }

    long fd_after = load_simple_prog();
    CHECK(fd_after >= 0, "load prog after closing all previous");
    if (fd_after >= 0) {
        uint32_t close_fd = (uint32_t)fd_after;
        raw_bpf(BPF_OBJ_CLOSE, &close_fd, sizeof(close_fd));
    }
}

static void test_multiple_perf_events(void) {
    MODULE_START("multiple_perf_events");

    long fds[4];
    int opened = 0;
    for (int i = 0; i < 4; i++) {
        fds[i] = open_perf_event_software();
        if (fds[i] < 0) break;
        opened++;
    }
    CHECK(opened == 4, "open 4 perf events");
    for (int i = 0; i < opened; i++) {
        CHECK(fds[i] >= 100, "perf event fd >= 100");
    }
    for (int i = 0; i < opened; i++) {
        close(fds[i]);
    }
}

static void test_bpf_small_size(void) {
    MODULE_START("bpf_small_size");

    struct {
        uint32_t prog_fd;
        uint32_t target_fd;
    } small = { .prog_fd = 0, .target_fd = 0 };
    long r = raw_bpf(BPF_LINK_CREATE, &small, sizeof(small));
    CHECK(r < 0, "BPF_LINK_CREATE with size < 20 returns error");

    uint32_t val = 0;
    r = raw_bpf(BPF_OBJ_CLOSE, &val, 0);
    CHECK(r < 0, "BPF_OBJ_CLOSE with size=0 returns error");

    struct {
        uint32_t target_fd;
        uint32_t attach_bpf_fd;
    } small_attach = { .target_fd = 0, .attach_bpf_fd = 0 };
    r = raw_bpf(BPF_PROG_ATTACH, &small_attach, sizeof(small_attach));
    CHECK(r < 0, "BPF_PROG_ATTACH with size < 16 returns error");
}

static void test_prog_attach_with_kprobe_perf(void) {
    MODULE_START("prog_attach_with_kprobe_perf");

    long prog_fd = load_simple_prog();
    CHECK(prog_fd >= 0, "load BPF program");
    if (prog_fd < 0) return;

    long perf_fd = open_perf_event_kprobe();
    CHECK(perf_fd >= 0, "open kprobe perf event");
    if (perf_fd < 0) { close(prog_fd); return; }

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
    CHECK(r == 0, "BPF_PROG_ATTACH to kprobe perf event succeeds");

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
    CHECK(r == 0, "BPF_PROG_DETACH from kprobe perf event succeeds");

    close(perf_fd);
    close(prog_fd);
}

int main(void) {
    printf("=== eBPF Attach / perf_event Test Suite ===\n");

    test_perf_event_open_software();
    test_perf_event_open_kprobe();
    test_perf_event_open_null_attr();
    test_perf_event_open_invalid_type();
    test_prog_attach_basic();
    test_prog_attach_invalid_fd();
    test_link_create_basic();
    test_link_create_invalid_fd();
    test_obj_close_map();
    test_obj_close_prog();
    test_obj_close_invalid();
    test_full_lifecycle();
    test_multiple_prog_load_close();
    test_multiple_perf_events();
    test_bpf_small_size();
    test_prog_attach_with_kprobe_perf();

    SUMMARY();
}
