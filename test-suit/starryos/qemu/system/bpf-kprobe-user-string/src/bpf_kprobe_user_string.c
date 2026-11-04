/*
 * bpf_probe_read_user_str from a kprobe BPF program.
 *
 * kprobe handlers run in the breakpoint exception with local IRQs disabled,
 * where a user page fault cannot be serviced. The helper must copy resident
 * user strings and fail on a page that is not resident instead of faulting it
 * in. The reads below are issued from the probed syscall of this process.
 *
 * Two further regressions cover the helper's nofault contract:
 *  - a probe firing while an outer faultable user copy is in progress (a
 *    stat() on a cold path) must still get EFAULT for a non-resident page:
 *    the outer copy's user-access scope may not service the helper's fault;
 *  - the caller's size bounds the source read: a valid range ending at a page
 *    boundary succeeds even when the following page is inaccessible, with
 *    strncpy_from_user_nofault truncation and return semantics.
 *
 * A third regression covers the helper's length ABI with an 8 KiB map value
 * as destination: a size above the 4096-byte cap is refused with -EINVAL
 * before the destination is touched (no verifier proves the buffer holds
 * `size` bytes), a request at the cap is served across a page boundary with
 * strncpy_from_user_nofault truncation, and an empty string returns 1
 * because the count includes the NUL.
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
#include <sys/mman.h>
#include <sys/stat.h>
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
#define HELPER_PROBE_READ_USER_STR 114

#define PERF_TYPE_KPROBE 6
#define PERF_EVENT_IOC_ENABLE 0x2400u
#define PERF_EVENT_IOC_DISABLE 0x2401u
#define PERF_EVENT_IOC_SET_BPF 0x40042408u

#define MADV_DONTNEED 4

#define OUT_LEN 16
#define RET_UNSET 0x5a5a5a5a5a5a5a5aull

struct bpf_insn {
    uint8_t code;
    uint8_t regs;
    int16_t off;
    int32_t imm;
};

/* Array map value shared with the BPF program. */
struct probe_slot {
    uint64_t pid_tgid; /* only this thread's hits are recorded */
    uint64_t user_ptr; /* string the helper reads */
    uint32_t size;     /* helper size argument */
    uint32_t armed;    /* one-shot gate for the page-fault probe program */
    uint64_t ret;      /* helper return value */
    char out[OUT_LEN]; /* helper output */
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

static const char resident_string[] = "starry-kprobe";

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

static int create_slot_map(void) {
    struct {
        uint32_t map_type;
        uint32_t key_size;
        uint32_t value_size;
        uint32_t max_entries;
        uint32_t map_flags;
    } attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = sizeof(uint32_t),
        .value_size = sizeof(struct probe_slot),
        .max_entries = 1,
    };
    return (int)sys_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
}

/* Destination map for reads larger than the BPF stack can hold. */
#define BIG_VALUE_LEN 8192

static int create_big_dst_map(void) {
    struct {
        uint32_t map_type;
        uint32_t key_size;
        uint32_t value_size;
        uint32_t max_entries;
        uint32_t map_flags;
    } attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = sizeof(uint32_t),
        .value_size = BIG_VALUE_LEN,
        .max_entries = 1,
    };
    return (int)sys_bpf(BPF_MAP_CREATE, &attr, sizeof(attr));
}

static int map_access_buf(int cmd, int map_fd, void *key, void *value) {
    struct {
        uint32_t map_fd;
        uint32_t pad;
        uint64_t key;
        uint64_t value;
        uint64_t flags;
    } attr = {
        .map_fd = (uint32_t)map_fd,
        .key = (uint64_t)(uintptr_t)key,
        .value = (uint64_t)(uintptr_t)value,
    };
    return (int)sys_bpf(cmd, &attr, sizeof(attr));
}

static int slot_access(int cmd, int map_fd, struct probe_slot *slot) {
    uint32_t key = 0;
    return map_access_buf(cmd, map_fd, &key, slot);
}

/* Byte the big destination is pre-filled with before each read. */
#define FILL 0xaa

/*
 * Run the probed syscall once with program C reading `size` bytes of the
 * string at `ptr` into the big map value, pre-filled with FILL; return the
 * slot and the value as left by the helper.
 */
static int read_into_big_value(int map_fd, int big_map_fd, const void *ptr,
                               uint32_t size, uint64_t pid_tgid,
                               struct probe_slot *result, unsigned char *out) {
    struct probe_slot slot;
    uint32_t key = 0;
    memset(&slot, 0, sizeof(slot));
    slot.pid_tgid = pid_tgid;
    slot.user_ptr = (uint64_t)(uintptr_t)ptr;
    slot.size = size;
    slot.ret = RET_UNSET;
    if (slot_access(BPF_MAP_UPDATE_ELEM, map_fd, &slot) != 0) {
        printf("map update failed errno=%d\n", errno);
        return -1;
    }
    memset(out, FILL, BIG_VALUE_LEN);
    if (map_access_buf(BPF_MAP_UPDATE_ELEM, big_map_fd, &key, out) != 0) {
        printf("big dst update failed errno=%d\n", errno);
        return -1;
    }
    if (syscall(SYS_getpid) < 0) {
        printf("getpid failed errno=%d\n", errno);
        return -1;
    }
    if (slot_access(BPF_MAP_LOOKUP_ELEM, map_fd, result) != 0 ||
        map_access_buf(BPF_MAP_LOOKUP_ELEM, big_map_fd, &key, out) != 0) {
        printf("map lookup failed errno=%d\n", errno);
        return -1;
    }
    if (result->ret == RET_UNSET) {
        printf("big-value probe program did not run for this thread\n");
        return -1;
    }
    return 0;
}

/*
 * Program A (attached to the getpid kprobe): if this thread hit the probe,
 * read the user string at slot->user_ptr with slot->size into the stack and
 * store the return value and bytes.
 */
static int load_program_a(int map_fd) {
    struct bpf_insn prog[] = {
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, map_fd),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 16, 0), /* -> exit */
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 7, 0, 0, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_GET_CURRENT_PID_TGID),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 7, 0, 0),
        insn(BPF_JMP | BPF_JNE | BPF_X, 0, 1, 12, 0), /* -> exit */
        insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -24, 0),
        insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -16, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 3, 7, 8, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 1, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 1, 0, 0, -24),
        insn(BPF_LDX | BPF_MEM | BPF_W, 2, 7, 16, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_PROBE_READ_USER_STR),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 0, 24, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 10, -24, 0),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 1, 32, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 10, -16, 0),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 1, 40, 0),
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

/*
 * Program B (attached to a kprobe on the page-fault entry): like program A,
 * but gated by slot->armed so only the first matching hit calls the helper.
 * The gate is cleared before the call, which keeps a fault raised by the
 * helper's own read (it re-enters the page-fault path) from recursing into
 * the helper again.
 */
static int load_program_b(int map_fd) {
    struct bpf_insn prog[] = {
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, map_fd),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 19, 0), /* -> exit */
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 7, 0, 0, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_GET_CURRENT_PID_TGID),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 7, 0, 0),
        insn(BPF_JMP | BPF_JNE | BPF_X, 0, 1, 15, 0), /* -> exit */
        insn(BPF_LDX | BPF_MEM | BPF_W, 1, 7, 20, 0),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 1, 0, 13, 0), /* armed == 0 -> exit */
        insn(BPF_ST | BPF_MEM | BPF_W, 7, 0, 20, 0),
        insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -24, 0),
        insn(BPF_ST | BPF_MEM | BPF_DW, 10, 0, -16, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 3, 7, 8, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 1, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 1, 0, 0, -24),
        insn(BPF_LDX | BPF_MEM | BPF_W, 2, 7, 16, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_PROBE_READ_USER_STR),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 0, 24, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 10, -24, 0),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 1, 32, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 10, -16, 0),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 1, 40, 0),
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

/*
 * Program C (attached to the getpid kprobe): like program A, but the helper's
 * destination is an 8 KiB array-map value, so sizes far beyond the BPF stack
 * can be served. The size comes from the slot and passes to the helper
 * unshortened, as Linux passes it to strncpy_from_user_nofault.
 */
static int load_program_c(int map_fd, int dst_map_fd) {
    struct bpf_insn prog[] = {
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, map_fd),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ST | BPF_MEM | BPF_W, 10, 0, -4, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 16, 0), /* -> exit */
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 7, 0, 0, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_GET_CURRENT_PID_TGID),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 1, 7, 0, 0),
        insn(BPF_JMP | BPF_JNE | BPF_X, 0, 1, 12, 0), /* -> exit */
        insn(BPF_LD | BPF_DW | BPF_IMM, 1, BPF_PSEUDO_MAP_FD, 0, dst_map_fd),
        insn(0, 0, 0, 0, 0),
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 2, 10, 0, 0),
        insn(BPF_ALU64 | BPF_ADD | BPF_K, 2, 0, 0, -4),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_MAP_LOOKUP_ELEM),
        insn(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 6, 0), /* -> exit */
        insn(BPF_ALU64 | BPF_MOV | BPF_X, 1, 0, 0, 0),
        insn(BPF_LDX | BPF_MEM | BPF_DW, 3, 7, 8, 0),
        insn(BPF_LDX | BPF_MEM | BPF_W, 2, 7, 16, 0),
        insn(BPF_JMP | BPF_CALL, 0, 0, 0, HELPER_PROBE_READ_USER_STR),
        insn(BPF_STX | BPF_MEM | BPF_DW, 7, 0, 24, 0),
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

static int open_kprobe(const char *symbol) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_KPROBE;
    attr.size = sizeof(attr);
    attr.config1 = (uint64_t)(uintptr_t)symbol;
    return (int)syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0ul);
}

static int attach_kprobe(int event_fd, int prog_fd) {
    if (ioctl(event_fd, PERF_EVENT_IOC_SET_BPF, prog_fd) != 0) {
        printf("attach kprobe failed errno=%d\n", errno);
        return -1;
    }
    if (ioctl(event_fd, PERF_EVENT_IOC_ENABLE, 0) != 0) {
        printf("enable kprobe failed errno=%d\n", errno);
        return -1;
    }
    return 0;
}

/* Run the probed syscall once with the helper reading `ptr`. */
static int read_through_probe(int map_fd, const void *ptr, uint32_t size,
                              uint64_t pid_tgid, struct probe_slot *result) {
    struct probe_slot slot;
    memset(&slot, 0, sizeof(slot));
    slot.pid_tgid = pid_tgid;
    slot.user_ptr = (uint64_t)(uintptr_t)ptr;
    slot.size = size;
    slot.ret = RET_UNSET;
    if (slot_access(BPF_MAP_UPDATE_ELEM, map_fd, &slot) != 0) {
        printf("map update failed errno=%d\n", errno);
        return -1;
    }
    if (syscall(SYS_getpid) < 0) {
        printf("getpid failed errno=%d\n", errno);
        return -1;
    }
    memset(result, 0, sizeof(*result));
    if (slot_access(BPF_MAP_LOOKUP_ELEM, map_fd, result) != 0) {
        printf("map lookup failed errno=%d\n", errno);
        return -1;
    }
    if (result->ret == RET_UNSET) {
        printf("probe program did not run for this thread\n");
        return -1;
    }
    return 0;
}

/*
 * Fire the page-fault-entry probe while an outer faultable user copy is in
 * progress: stat() reads the path from a page whose mapping was just dropped
 * with MADV_DONTNEED, so the copy faults with its user-access scope held and
 * the kprobe on the page-fault entry runs inside that window. The armed
 * helper read targets a never-touched page and must fail with EFAULT without
 * the outer scope servicing the fault.
 */
static int probe_inside_faultable_user_copy(int map_fd, uint64_t pid_tgid,
                                            struct probe_slot *result) {
    void *cold = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (cold == MAP_FAILED) {
        printf("mmap cold failed errno=%d\n", errno);
        return -1;
    }
    char *path_page = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (path_page == MAP_FAILED) {
        printf("mmap path failed errno=%d\n", errno);
        munmap(cold, 4096);
        return -1;
    }
    memset(path_page, 'p', 16);
    path_page[16] = '\0';
    /* Drop the mapping's pages so stat()'s path copy faults under its
     * user-access scope; the page refaults as zero-filled, giving an empty
     * path and ENOENT. */
    if (madvise(path_page, 4096, MADV_DONTNEED) != 0) {
        printf("madvise failed errno=%d\n", errno);
        munmap(cold, 4096);
        munmap(path_page, 4096);
        return -1;
    }

    struct probe_slot slot;
    memset(&slot, 0, sizeof(slot));
    slot.pid_tgid = pid_tgid;
    slot.user_ptr = (uint64_t)(uintptr_t)cold;
    slot.size = 8;
    slot.armed = 1;
    slot.ret = RET_UNSET;
    if (slot_access(BPF_MAP_UPDATE_ELEM, map_fd, &slot) != 0) {
        printf("map update failed errno=%d\n", errno);
        munmap(cold, 4096);
        return -1;
    }

    struct stat st;
    if (stat(path_page, &st) == 0) {
        printf("stat of an empty path unexpectedly succeeded\n");
        munmap(cold, 4096);
        munmap(path_page, 4096);
        return -1;
    }

    memset(result, 0, sizeof(*result));
    if (slot_access(BPF_MAP_LOOKUP_ELEM, map_fd, result) != 0) {
        printf("map lookup failed errno=%d\n", errno);
        munmap(cold, 4096);
        munmap(path_page, 4096);
        return -1;
    }
    munmap(cold, 4096);
    munmap(path_page, 4096);
    if (result->ret == RET_UNSET) {
        printf("page-fault probe program did not run\n");
        return -1;
    }
    return 0;
}

int main(void) {
    char symbol[400];
    int failures = 0;

    if (find_kernel_symbol("sys_getpid", symbol, sizeof(symbol)) != 0) {
        return 1;
    }
    int map_fd = create_slot_map();
    if (map_fd < 0) {
        printf("BPF_MAP_CREATE failed errno=%d\n", errno);
        return 1;
    }
    int prog_fd = load_program_a(map_fd);
    if (prog_fd < 0) {
        printf("BPF_PROG_LOAD failed errno=%d\n", errno);
        return 1;
    }
    int event_fd = open_kprobe(symbol);
    if (event_fd < 0) {
        printf("perf_event_open(kprobe %s) failed errno=%d\n", symbol, errno);
        return 1;
    }
    if (attach_kprobe(event_fd, prog_fd) != 0) {
        return 1;
    }

    uint64_t pid_tgid = ((uint64_t)getpid() << 32) | (uint64_t)syscall(SYS_gettid);
    struct probe_slot result;

    if (read_through_probe(map_fd, resident_string, OUT_LEN, pid_tgid, &result) != 0) {
        return 1;
    }
    if (result.ret != sizeof(resident_string) ||
        memcmp(result.out, resident_string, sizeof(resident_string)) != 0) {
        printf("resident string: ret=%lld out=%.16s\n", (long long)result.ret,
               result.out);
        failures++;
    }

    /* A fresh anonymous page that user space never touched is not resident. */
    void *cold = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (cold == MAP_FAILED) {
        printf("mmap failed errno=%d\n", errno);
        return 1;
    }
    if (read_through_probe(map_fd, cold, OUT_LEN, pid_tgid, &result) != 0) {
        return 1;
    }
    /* Helper errors reach the program as negative errnos; older kernels the
     * test suite runs against reported the positive value. */
    int64_t ret = (int64_t)result.ret;
    if (ret != EFAULT && ret != -EFAULT) {
        printf("non-resident page: ret=%lld, expected EFAULT\n", (long long)ret);
        failures++;
    }
    munmap(cold, 4096);

    /*
     * Cross-page reads: two adjacent resident pages. The string starts 8
     * bytes before the first page ends and its terminator lies on the second
     * page, so a read bounded by size=16 spans the boundary and must succeed
     * like Linux's strncpy_from_user_nofault for a valid requested range.
     */
    unsigned char *pages = mmap(NULL, 8192, PROT_READ | PROT_WRITE,
                                MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (pages == MAP_FAILED) {
        printf("mmap pages failed errno=%d\n", errno);
        return 1;
    }
    memset(pages, 0, 8192); /* fault both pages in; they are resident now */
    const char *tail = "12345678";
    const char *head = "ABCDE";
    memcpy(pages + 4096 - 8, tail, 8);
    memcpy(pages + 4096, head, 5);
    pages[4096 + 5] = '\0';

    if (read_through_probe(map_fd, pages + 4096 - 8, 16, pid_tgid, &result) != 0) {
        return 1;
    }
    if (result.ret != 14 || memcmp(result.out, "12345678ABCDE", 14) != 0) {
        printf("cross-page string: ret=%lld out=%.16s\n", (long long)result.ret,
               result.out);
        failures++;
    }

    /*
     * Truncation at a page boundary: make the second page inaccessible. The
     * eight non-NUL bytes at the end of the first page are the whole requested
     * range (size=8); the result must be the truncation, not EFAULT, and the
     * bytes past the requested range must never be read.
     */
    if (mprotect(pages + 4096, 4096, PROT_NONE) != 0) {
        printf("mprotect failed errno=%d\n", errno);
        return 1;
    }
    if (read_through_probe(map_fd, pages + 4096 - 8, 8, pid_tgid, &result) != 0) {
        return 1;
    }
    if (result.ret != 8 || memcmp(result.out, "1234567", 8) != 0) {
        printf("truncated tail: ret=%lld out=%.16s\n", (long long)result.ret,
               result.out);
        failures++;
    }
    mprotect(pages + 4096, 4096, PROT_READ | PROT_WRITE);
    munmap(pages, 8192);

    if (ioctl(event_fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        printf("disable kprobe failed errno=%d\n", errno);
        failures++;
    }
    close(event_fd);
    close(prog_fd);

    /*
     * Length ABI: program C reads into an 8 KiB array-map value pre-filled
     * with FILL, so every byte the helper writes, and every byte it must not
     * write, can be checked.
     */
    int big_map_fd = create_big_dst_map();
    if (big_map_fd < 0) {
        printf("BPF_MAP_CREATE (big dst) failed errno=%d\n", errno);
        return 1;
    }
    int prog_c_fd = load_program_c(map_fd, big_map_fd);
    if (prog_c_fd < 0) {
        printf("BPF_PROG_LOAD (big dst probe) failed errno=%d\n", errno);
        return 1;
    }
    if (find_kernel_symbol("sys_getpid", symbol, sizeof(symbol)) != 0) {
        return 1;
    }
    int big_event_fd = open_kprobe(symbol);
    if (big_event_fd < 0) {
        printf("perf_event_open(kprobe %s) failed errno=%d\n", symbol, errno);
        return 1;
    }
    if (attach_kprobe(big_event_fd, prog_c_fd) != 0) {
        return 1;
    }

    unsigned char *big = mmap(NULL, 12288, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (big == MAP_FAILED) {
        printf("mmap big failed errno=%d\n", errno);
        return 1;
    }
    for (size_t i = 0; i < 5000; i++) {
        big[i] = (unsigned char)('a' + (i % 26));
    }
    big[5000] = '\0';

    static unsigned char big_out[BIG_VALUE_LEN];

    /*
     * Beyond the cap the request is refused before either range is touched:
     * nothing proves the destination holds `size` bytes, so a larger size
     * could otherwise write or clear past the program's buffer.
     */
    if (read_into_big_value(map_fd, big_map_fd, big, 6000, pid_tgid, &result,
                            big_out) != 0) {
        return 1;
    }
    if ((int64_t)result.ret != -EINVAL) {
        printf("oversized request: ret=%lld, expected -EINVAL\n",
               (long long)result.ret);
        failures++;
    }
    for (size_t i = 0; i < BIG_VALUE_LEN; i++) {
        if (big_out[i] != FILL) {
            printf("oversized request: destination byte %zu was touched\n", i);
            failures++;
            break;
        }
    }

    /*
     * A request at the cap is served in full: 4096 bytes starting 100 bytes
     * into the string cross the page boundary, find no NUL, and truncate with
     * a forced terminator at the last byte; the rest of the value is untouched.
     */
    if (read_into_big_value(map_fd, big_map_fd, big + 100, 4096, pid_tgid,
                            &result, big_out) != 0) {
        return 1;
    }
    if (result.ret != 4096 || memcmp(big_out, big + 100, 4095) != 0 ||
        big_out[4095] != '\0') {
        printf("request at the cap: ret=%lld first=%c last=0x%02x\n",
               (long long)result.ret, big_out[0], big_out[4095]);
        failures++;
    }
    for (size_t i = 4096; i < BIG_VALUE_LEN; i++) {
        if (big_out[i] != FILL) {
            printf("request at the cap: byte %zu past the size was touched\n", i);
            failures++;
            break;
        }
    }

    /*
     * Empty string: strncpy_from_user_nofault returns the copied length
     * including the NUL, so the helper returns 1 and stores the terminator
     * over the pre-filled destination.
     */
    big[8192] = '\0'; /* resident third page */
    if (read_into_big_value(map_fd, big_map_fd, big + 8192, 16, pid_tgid,
                            &result, big_out) != 0) {
        return 1;
    }
    if (result.ret != 1 || big_out[0] != '\0') {
        printf("empty string: ret=%lld first=0x%02x\n", (long long)result.ret,
               big_out[0]);
        failures++;
    }

    if (ioctl(big_event_fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        printf("disable big kprobe failed errno=%d\n", errno);
        failures++;
    }
    close(big_event_fd);
    close(prog_c_fd);
    close(big_map_fd);
    munmap(big, 12288);

    /*
     * The interrupted-copy regression needs a kprobe at the page-fault entry,
     * which the getpid probe above never reaches. It uses its own program so
     * the armed gate does not disturb the scenarios above.
     */
    if (find_kernel_symbol("handle_page_fault", symbol, sizeof(symbol)) != 0) {
        return 1;
    }
    int prog_b_fd = load_program_b(map_fd);
    if (prog_b_fd < 0) {
        printf("BPF_PROG_LOAD (fault probe) failed errno=%d\n", errno);
        return 1;
    }
    int fault_event_fd = open_kprobe(symbol);
    if (fault_event_fd < 0) {
        printf("perf_event_open(kprobe %s) failed errno=%d\n", symbol, errno);
        return 1;
    }
    if (attach_kprobe(fault_event_fd, prog_b_fd) != 0) {
        return 1;
    }
    if (probe_inside_faultable_user_copy(map_fd, pid_tgid, &result) != 0) {
        return 1;
    }
    if ((int64_t)result.ret != -EFAULT) {
        printf("interrupted copy: ret=%lld, expected -EFAULT\n",
               (long long)result.ret);
        failures++;
    }
    if (ioctl(fault_event_fd, PERF_EVENT_IOC_DISABLE, 0) != 0) {
        printf("disable fault kprobe failed errno=%d\n", errno);
        failures++;
    }
    close(fault_event_fd);
    close(prog_b_fd);
    close(map_fd);

    if (failures != 0) {
        return 1;
    }
    printf("STARRY_BPF_KPROBE_USER_STRING_OK\n");
    return 0;
}
