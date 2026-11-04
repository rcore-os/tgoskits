/*
 * kprobe registration and removal through perf_event_open.
 *
 * Installing or removing a kprobe patches kernel text on every CPU. Each
 * probe here counts the hits of its own syscall from this thread through a
 * BPF program; a probe must count exactly the calls made while it is
 * installed and leave the syscall unaffected after it is closed. Probes are
 * installed and removed one after another, and two probes on different
 * functions are also kept installed together.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef SYS_bpf
#error "SYS_bpf is required"
#endif
#ifndef SYS_perf_event_open
#error "SYS_perf_event_open is required"
#endif

#define BPF_MAP_CREATE 0
#define BPF_MAP_LOOKUP_ELEM 1
#define BPF_MAP_UPDATE_ELEM 2
#define BPF_PROG_LOAD 5
#define BPF_MAP_TYPE_ARRAY 2
#define BPF_PROG_TYPE_KPROBE 2
#define BPF_PSEUDO_MAP_FD 1

#define BPF_LD 0x00
#define BPF_LDX 0x01
#define BPF_ST 0x02
#define BPF_STX 0x03
#define BPF_JMP 0x05
#define BPF_ALU64 0x07
#define BPF_W 0x00
#define BPF_DW 0x18
#define BPF_IMM 0x00
#define BPF_MEM 0x60
#define BPF_K 0x00
#define BPF_X 0x08
#define BPF_ADD 0x00
#define BPF_MOV 0xb0
#define BPF_JEQ 0x10
#define BPF_JNE 0x50
#define BPF_CALL 0x80
#define BPF_EXIT 0x90

#define HELPER_MAP_LOOKUP_ELEM 1
#define HELPER_GET_CURRENT_PID_TGID 14

#define PERF_TYPE_KPROBE 6
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_SET_BPF 0x40042408u

#define SEQUENTIAL_CYCLES 8
#define CALLS_PER_CHECK 3

struct bpf_insn {
    uint8_t code;
    uint8_t regs;
    int16_t off;
    int32_t imm;
};

/* Array map value: hits of the probed function from `pid_tgid`. */
struct hit_slot {
    uint64_t pid_tgid;
    uint64_t hits;
};

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    uint32_t wakeup_events;
    uint32_t bp_type;
    uint64_t config1;
    uint64_t config2;
};

struct probe {
    const char *suffix;
    long sysno;
    char symbol[400];
    int map_fd;
    int prog_fd;
};

static uint64_t self_pid_tgid;

static long sys_bpf(int cmd, void *attr, unsigned int size) {
    return syscall(SYS_bpf, cmd, attr, size);
}

static struct bpf_insn insn(uint8_t code, uint8_t dst, uint8_t src,
                            int16_t off, int32_t imm) {
    struct bpf_insn i = {
        .code = code,
        .regs = (uint8_t)((dst & 0xf) | ((src & 0xf) << 4)),
        .off = off,
        .imm = imm,
    };
    return i;
}

/* Kernel symbols are mangled; pick the entry whose name ends in `suffix`. */
static int find_kernel_symbol(const char *suffix, char *name, size_t len) {
    FILE *f = fopen("/proc/kallsyms", "r");
    if (f == NULL) {
        printf("open /proc/kallsyms failed errno=%d\n", errno);
        return -1;
    }
    char line[512];
    size_t suffix_len = strlen(suffix);
    int found = -1;
    while (fgets(line, sizeof(line), f) != NULL) {
        unsigned long long addr;
        char kind;
        char sym[400];
        if (sscanf(line, "%llx %c %399s", &addr, &kind, sym) != 3) {
            continue;
        }
        size_t sym_len = strlen(sym);
        if ((kind == 't' || kind == 'T') && addr != 0 && sym_len >= suffix_len &&
            strcmp(sym + sym_len - suffix_len, suffix) == 0) {
            snprintf(name, len, "%s", sym);
            found = 0;
            break;
        }
    }
    fclose(f);
    if (found != 0) {
        printf("no kernel symbol ending in %s\n", suffix);
    }
    return found;
}

static int slot_access(int cmd, int map_fd, struct hit_slot *slot) {
    uint32_t key = 0;
    struct {
        uint32_t map_fd;
        uint32_t pad;
        uint64_t key;
        uint64_t value;
        uint64_t flags;
    } attr = {
        .map_fd = (uint32_t)map_fd,
        .key = (uint64_t)(uintptr_t)&key,
        .value = (uint64_t)(uintptr_t)slot,
    };
    return (int)sys_bpf(cmd, &attr, sizeof(attr));
}

/* r0 = lookup(map, 0); if this thread hit the probe, r0->hits += 1 */
static int load_counter(int map_fd) {
    struct bpf_insn prog[] = {
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, map_fd),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 7, 0), /* -> exit */
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 7, 0, 0, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_GET_CURRENT_PID_TGID),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 7, 0, 0),
        insn(BPF_JMP | BPF_JNE | BPF_X, 0, 1, 3, 0), /* -> exit */
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 7, 8, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 1, 0, 0, 1),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 1, 8, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_K, 0, 0, 0, 0),
        insn(BPF_JMP | BPF_EXIT, 0, 0, 0, 0),
    };
    static const char license[] = "GPL";
    struct {
        uint32_t prog_type;
        uint32_t insn_cnt;
        uint64_t insns;
        uint64_t license;
        uint32_t log_level;
        uint32_t log_size;
        uint64_t log_buf;
        uint32_t kern_version;
        uint32_t prog_flags;
    } attr = {
        .prog_type = BPF_PROG_TYPE_KPROBE,
        .insn_cnt = sizeof(prog) / sizeof(prog[0]),
        .insns = (uint64_t)(uintptr_t)prog,
        .license = (uint64_t)(uintptr_t)license,
    };
    return (int)sys_bpf(BPF_PROG_LOAD, &attr, sizeof(attr));
}

static int prepare_probe(struct probe *p) {
    if (find_kernel_symbol(p->suffix, p->symbol, sizeof(p->symbol)) != 0) {
        return -1;
    }
    struct {
        uint32_t map_type;
        uint32_t key_size;
        uint32_t value_size;
        uint32_t max_entries;
        uint32_t map_flags;
    } attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = sizeof(uint32_t),
        .value_size = sizeof(struct hit_slot),
        .max_entries = 1,
    };
    p->map_fd = (int)sys_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
    if (p->map_fd < 0) {
        printf("BPF_MAP_CREATE failed errno=%d\n", errno);
        return -1;
    }
    struct hit_slot slot = {.pid_tgid = self_pid_tgid, .hits = 0};
    if (slot_access(BPF_MAP_UPDATE_ELEM, p->map_fd, &slot) != 0) {
        printf("map update failed errno=%d\n", errno);
        return -1;
    }
    p->prog_fd = load_counter(p->map_fd);
    if (p->prog_fd < 0) {
        printf("BPF_PROG_LOAD failed errno=%d\n", errno);
        return -1;
    }
    return 0;
}

/* Installs the probe; returns the perf fd that owns it. */
static int install(const struct probe *p) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_KPROBE;
    attr.size = sizeof(attr);
    attr.config1 = (uint64_t)(uintptr_t)p->symbol;
    int fd = (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        printf("perf_event_open(kprobe %s) failed errno=%d\n", p->symbol, errno);
        return -1;
    }
    if (ioctl(fd, PERF_EVENT_IOC_SET_BPF, p->prog_fd) != 0 ||
        ioctl(fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        printf("attach or enable kprobe %s failed errno=%d\n", p->symbol, errno);
        close(fd);
        return -1;
    }
    return fd;
}

static int64_t hits(const struct probe *p) {
    struct hit_slot slot;
    if (slot_access(BPF_MAP_LOOKUP_ELEM, p->map_fd, &slot) != 0) {
        printf("map lookup failed errno=%d\n", errno);
        return -1;
    }
    return (int64_t)slot.hits;
}

/* Calls the probed syscall and checks that the probe saw `expected` of them. */
static int expect_hits(const struct probe *p, int64_t expected, const char *when) {
    int64_t before = hits(p);
    for (int i = 0; i < CALLS_PER_CHECK; i++) {
        if (syscall(p->sysno) < 0) {
            printf("syscall %ld failed errno=%d\n", p->sysno, errno);
            return 1;
        }
    }
    int64_t seen = hits(p) - before;
    if (before < 0 || seen != expected) {
        printf("%s %s: %lld hits for %d calls, expected %lld\n", p->suffix, when,
               (long long)seen, CALLS_PER_CHECK, (long long)expected);
        return 1;
    }
    return 0;
}

int main(void) {
    struct probe pid = {.suffix = "sys_getpid", .sysno = SYS_getpid};
    struct probe ppid = {.suffix = "sys_getppid", .sysno = SYS_getppid};
    int failures = 0;

    self_pid_tgid = ((uint64_t)getpid() << 32) | (uint64_t)syscall(SYS_gettid);
    if (prepare_probe(&pid) != 0 || prepare_probe(&ppid) != 0) {
        return 1;
    }

    for (int cycle = 0; cycle < SEQUENTIAL_CYCLES; cycle++) {
        int fd = install(&pid);
        if (fd < 0) {
            return 1;
        }
        failures += expect_hits(&pid, CALLS_PER_CHECK, "installed");
        close(fd);
        failures += expect_hits(&pid, 0, "removed");
    }

    int pid_fd = install(&pid);
    int ppid_fd = install(&ppid);
    if (pid_fd < 0 || ppid_fd < 0) {
        return 1;
    }
    failures += expect_hits(&pid, CALLS_PER_CHECK, "installed with another probe");
    failures += expect_hits(&ppid, CALLS_PER_CHECK, "installed with another probe");
    close(pid_fd);
    failures += expect_hits(&pid, 0, "removed before another probe");
    failures += expect_hits(&ppid, CALLS_PER_CHECK, "kept after another probe was removed");
    close(ppid_fd);
    failures += expect_hits(&ppid, 0, "removed");

    if (failures != 0) {
        return 1;
    }
    printf("STARRY_KPROBE_PERF_REGISTER_OK\n");
    return 0;
}
