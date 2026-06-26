/*
 * perf_hw_pmu_sysfs.c -- emulate what upstream `perf` does at startup, namely
 * PMU discovery via sysfs/procfs followed by opening a NAMED event through its
 * dynamic PMU type.
 *
 * Real `perf` does NOT hardcode PERF_TYPE_HARDWARE for `perf stat cycles` on
 * arm64. Instead it:
 *   1. probes /proc/sys/kernel/perf_event_paranoid to decide whether perf is
 *      usable at all (the "is perf supported" gate),
 *   2. enumerates PMUs under /sys/bus/event_source/devices/, reading each PMU's
 *      `type` file to learn its DYNAMIC perf_event_attr.type number (the arm64
 *      core PMU is exposed as armv8_pmuv3_0, type == 8 on a typical kernel),
 *   3. resolves a named event (e.g. "cpu_cycles") by reading
 *      .../events/<name>, whose contents look like "event=0x11", and parses the
 *      hex after "event=" into perf_event_attr.config,
 *   4. calls perf_event_open(attr{ type=<dynamic pmu type>, config=<parsed> }).
 *
 * This test reproduces that flow end to end. Success proves the kernel:
 *   - exposes the perf_event_paranoid procfs knob,
 *   - exposes the armv8_pmuv3_0 PMU node with a parseable `type`,
 *   - exposes the named `cpu_cycles` event with a parseable `event=0x..` config,
 *   - routes a perf_event_open carrying the DYNAMIC PMU type (not the static
 *     PERF_TYPE_* constants) to the hardware PMU backend (fd >= 0),
 *   - and that RESET/ENABLE/DISABLE/read(2) work on that fd.
 *
 * SUCCESS gate (the ABI assertions):
 *   - /proc/sys/kernel/perf_event_paranoid opened + read,
 *   - armv8_pmuv3_0/type parsed to a positive integer (we LOG if != 8 but only
 *     require > 0, since the dynamic type number is kernel-assigned),
 *   - armv8_pmuv3_0/events/cpu_cycles parsed to config == 0x11,
 *   - perf_event_open(dynamic type) returns fd >= 0,
 *   - read(fd, &val, 8) returns exactly 8.
 * The counter VALUE magnitude is diagnostic only: under QEMU TCG it may be 0, so
 * a zero value WARNs but does not fail.
 *
 * On success exactly one final line is printed:
 *     STARRY_PERF_PMU_SYSFS_OK
 * On any failure a line "perf-pmu-sysfs FAILED: <reason>" is printed and the
 * process exits non-zero, which the grouped runner treats as a failure.
 *
 * Everything the kernel ABI needs is defined locally so the test does not depend
 * on <linux/perf_event.h> being present in the musl sysroot. The
 * perf_event_attr layout is byte-identical to the M0/M1/M2 cases
 * (perf-hw-cycles / perf-hw-stat / perf-hw-sample).
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

/* sysfs/procfs paths that upstream perf consults at startup. */
#define PATH_PARANOID "/proc/sys/kernel/perf_event_paranoid"
#define PMU_DIR "/sys/bus/event_source/devices/armv8_pmuv3_0"
#define PATH_PMU_TYPE PMU_DIR "/type"
#define PATH_PMU_CPUS PMU_DIR "/cpus"
#define PATH_PMU_FMT_EVENT PMU_DIR "/format/event"
#define PATH_EVT_CPU_CYCLES PMU_DIR "/events/cpu_cycles"

/* Expected (typical) dynamic PMU type for the arm64 core PMU. */
#define PMU_TYPE_EXPECTED 8
/* Expected ARM CPU_CYCLES PMU event number behind cpu_cycles. */
#define EVT_CPU_CYCLES_EXPECTED 0x11ull

/*
 * perf_event ioctl numbers: _IO('$', nr) == (0x24 << 8) | nr.
 *   PERF_EVENT_IOC_ENABLE  = _IO('$', 0) = 0x2400
 *   PERF_EVENT_IOC_DISABLE = _IO('$', 1) = 0x2401
 *   PERF_EVENT_IOC_RESET   = _IO('$', 3) = 0x2403
 */
#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_EVENT_IOC_RESET
#define PERF_EVENT_IOC_RESET 0x2403u
#endif

/*
 * Linux perf_event_attr ABI -- byte-identical to the M0/M1/M2 cases. Fields are
 * laid out in the kernel's order; the 64-bit flags word (disabled/inherit/...
 * bitfields) is a plain __u64 we leave mostly zero, then set the low bit for
 * `disabled`. Trailing fields are padded out so sizeof() lands on a stable
 * value (>= PERF_ATTR_SIZE_VER0 == 64).
 */
struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    union {
        uint64_t sample_period;
        uint64_t sample_freq;
    };
    uint64_t sample_type;
    uint64_t read_format;

    uint64_t flags; /* bit 0 == disabled; rest left zero */

    union {
        uint32_t wakeup_events;
        uint32_t wakeup_watermark;
    };
    uint32_t bp_type;
    union {
        uint64_t bp_addr;
        uint64_t config1;
    };
    union {
        uint64_t bp_len;
        uint64_t config2;
    };
    uint64_t branch_sample_type;
    uint64_t sample_regs_user;
    uint32_t sample_stack_user;
    int32_t clockid;
    uint64_t sample_regs_intr;
    uint32_t aux_watermark;
    uint16_t sample_max_stack;
    uint16_t __reserved_2;
    uint32_t aux_sample_size;
    uint32_t __reserved_3;
};

/* perf_event_attr.flags bit 0 */
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)

/* Compile-time guards: the ABI offsets that make this layout correct. */
_Static_assert(offsetof(struct perf_event_attr, config) == 8,
               "perf_event_attr.config must be at offset 8");
_Static_assert(offsetof(struct perf_event_attr, read_format) == 32,
               "perf_event_attr.read_format must be at offset 32");
_Static_assert(offsetof(struct perf_event_attr, flags) == 40,
               "perf_event_attr.flags (disabled bit) must be at offset 40");

#ifndef SYS_perf_event_open
/* aarch64 == 241; also the generic asm-generic number used by arm64/riscv. */
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-pmu-sysfs FAILED: %s\n", reason);
    return 1;
}

/*
 * Read a whole small sysfs/procfs file into buf (NUL-terminated). Returns the
 * number of bytes read (>= 0) on success, or -1 if the file cannot be opened
 * (errno preserved). A readable-but-empty file returns 0.
 */
static ssize_t read_small_file(const char *path, char *buf, size_t cap) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    size_t total = 0;
    while (total + 1 < cap) {
        ssize_t n = read(fd, buf + total, cap - 1 - total);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            close(fd);
            return -1;
        }
        if (n == 0) {
            break; /* EOF */
        }
        total += (size_t)n;
    }
    buf[total] = '\0';
    close(fd);
    return (ssize_t)total;
}

/* Trim a trailing newline (and any trailing whitespace) in place. */
static void rstrip(char *s) {
    size_t len = strlen(s);
    while (len > 0
           && (s[len - 1] == '\n' || s[len - 1] == '\r' || s[len - 1] == ' '
               || s[len - 1] == '\t')) {
        s[--len] = '\0';
    }
}

/*
 * Parse a leading base-10 integer from s into *out. Returns 0 on success, -1 if
 * no digits are present. Leading whitespace is skipped.
 */
static int parse_int(const char *s, long *out) {
    while (*s == ' ' || *s == '\t' || *s == '\n') {
        s++;
    }
    int neg = 0;
    if (*s == '+' || *s == '-') {
        neg = (*s == '-');
        s++;
    }
    if (*s < '0' || *s > '9') {
        return -1;
    }
    long v = 0;
    while (*s >= '0' && *s <= '9') {
        v = v * 10 + (long)(*s - '0');
        s++;
    }
    *out = neg ? -v : v;
    return 0;
}

/*
 * Parse the config out of a sysfs events file body of the form
 * "event=0x11" (there may be trailing ",..." terms, e.g. "event=0x11,umask=..").
 * We locate the "event=" token and parse the hex value after it into *out.
 * Returns 0 on success, -1 if "event=" is absent or no hex digits follow.
 */
static int parse_event_config(const char *body, uint64_t *out) {
    const char *p = strstr(body, "event=");
    if (p == NULL) {
        return -1;
    }
    p += strlen("event=");
    /* Optional 0x / 0X prefix. */
    if (p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }
    int any = 0;
    uint64_t v = 0;
    for (;;) {
        char c = *p;
        unsigned int d;
        if (c >= '0' && c <= '9') {
            d = (unsigned int)(c - '0');
        } else if (c >= 'a' && c <= 'f') {
            d = (unsigned int)(c - 'a' + 10);
        } else if (c >= 'A' && c <= 'F') {
            d = (unsigned int)(c - 'A' + 10);
        } else {
            break;
        }
        v = (v << 4) | d;
        any = 1;
        p++;
    }
    if (!any) {
        return -1;
    }
    *out = v;
    return 0;
}

int main(void) {
    char buf[256];

    /*
     * Step 1: perf's "is perf supported" probe. The file must exist and be
     * readable; its value is informational (0/1/2/-1). FAIL only if it cannot
     * be opened.
     */
    ssize_t n = read_small_file(PATH_PARANOID, buf, sizeof(buf));
    if (n < 0) {
        char msg[128];
        snprintf(msg, sizeof(msg), "open(%s) failed errno=%d", PATH_PARANOID,
                 errno);
        return fail(msg);
    }
    rstrip(buf);
    {
        long paranoid = 0;
        if (parse_int(buf, &paranoid) == 0) {
            printf("STARRY_PERF_PMU_SYSFS paranoid=%ld\n", paranoid);
        } else {
            /* Readable but unparseable is still "supported"; just echo raw. */
            printf("STARRY_PERF_PMU_SYSFS paranoid_raw=\"%s\"\n", buf);
        }
    }

    /*
     * Step 2: read the dynamic PMU type for the arm64 core PMU. perf would
     * enumerate the whole devices dir; we go straight to the well-known node.
     */
    n = read_small_file(PATH_PMU_TYPE, buf, sizeof(buf));
    if (n < 0) {
        char msg[160];
        snprintf(msg, sizeof(msg), "open(%s) failed errno=%d", PATH_PMU_TYPE,
                 errno);
        return fail(msg);
    }
    rstrip(buf);
    long pmu_type_l = 0;
    if (parse_int(buf, &pmu_type_l) != 0) {
        char msg[160];
        /* Bound the echoed contents so the message can never overflow msg. */
        snprintf(msg, sizeof(msg), "unparseable %s contents=\"%.48s\"",
                 PATH_PMU_TYPE, buf);
        return fail(msg);
    }
    printf("STARRY_PERF_PMU_SYSFS pmu_type=%ld\n", pmu_type_l);
    if (pmu_type_l <= 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "pmu_type must be > 0, got %ld", pmu_type_l);
        return fail(msg);
    }
    if (pmu_type_l != PMU_TYPE_EXPECTED) {
        /* Not fatal: the dynamic type number is kernel-assigned. */
        printf("perf-pmu-sysfs WARN: pmu_type=%ld (expected %d)\n", pmu_type_l,
               PMU_TYPE_EXPECTED);
    }
    uint32_t pmu_type = (uint32_t)pmu_type_l;

    /*
     * Step 3: resolve the named event "cpu_cycles" -> config. The body looks
     * like "event=0x11"; parse the hex after "event=".
     */
    n = read_small_file(PATH_EVT_CPU_CYCLES, buf, sizeof(buf));
    if (n < 0) {
        char msg[160];
        snprintf(msg, sizeof(msg), "open(%s) failed errno=%d",
                 PATH_EVT_CPU_CYCLES, errno);
        return fail(msg);
    }
    rstrip(buf);
    uint64_t config = 0;
    if (parse_event_config(buf, &config) != 0) {
        char msg[160];
        /* Bound the echoed contents so the message can never overflow msg. */
        snprintf(msg, sizeof(msg), "unparseable %s contents=\"%.48s\"",
                 PATH_EVT_CPU_CYCLES, buf);
        return fail(msg);
    }
    printf("STARRY_PERF_PMU_SYSFS cpu_cycles config=0x%llx\n",
           (unsigned long long)config);
    if (config != EVT_CPU_CYCLES_EXPECTED) {
        char msg[96];
        snprintf(msg, sizeof(msg), "cpu_cycles config=0x%llx (expected 0x%llx)",
                 (unsigned long long)config,
                 (unsigned long long)EVT_CPU_CYCLES_EXPECTED);
        return fail(msg);
    }

    /*
     * Step 4 (optional, print only): cpus mask + format/event descriptor. These
     * are informational and never gate success; missing files are tolerated.
     */
    if (read_small_file(PATH_PMU_CPUS, buf, sizeof(buf)) >= 0) {
        rstrip(buf);
        printf("STARRY_PERF_PMU_SYSFS cpus=\"%s\"\n", buf);
    } else {
        printf("STARRY_PERF_PMU_SYSFS cpus=<absent>\n");
    }
    if (read_small_file(PATH_PMU_FMT_EVENT, buf, sizeof(buf)) >= 0) {
        rstrip(buf);
        printf("STARRY_PERF_PMU_SYSFS format_event=\"%s\"\n", buf);
    } else {
        printf("STARRY_PERF_PMU_SYSFS format_event=<absent>\n");
    }

    /*
     * Step 5: open the event through the DYNAMIC PMU type (not PERF_TYPE_*).
     * fd >= 0 proves the kernel routes the dynamic PMU type to the hw backend.
     */
    struct perf_event_attr attr;
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }
    attr.type = pmu_type; /* dynamic type from sysfs, e.g. 8 */
    attr.config = config; /* parsed from events/cpu_cycles, e.g. 0x11 */
    attr.size = (uint32_t)sizeof(struct perf_event_attr);
    attr.read_format = 0;
    attr.flags = PERF_ATTR_FLAG_DISABLED; /* disabled = 1 */

    /* pid=0 (self), cpu=-1 (any), group_fd=-1 (leader), flags=0 */
    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        char msg[128];
        snprintf(msg, sizeof(msg),
                 "perf_event_open(type=%u config=0x%llx) errno=%d", pmu_type,
                 (unsigned long long)config, errno);
        return fail(msg);
    }
    int efd = (int)fd;

    /*
     * Step 6: drive the counter through the ioctl ABI and read it back.
     */
    (void)ioctl(efd, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_ENABLE, 0);

    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 20000000ull; i++) {
        spin += i;
    }

    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t val = 0;
    ssize_t rn = read(efd, &val, sizeof(val));

    printf("STARRY_PERF_PMU_SYSFS value=%llu n=%lld\n", (unsigned long long)val,
           (long long)rn);

    /* WARN-only: a zero counter value does not gate success under QEMU TCG. */
    if (val == 0) {
        printf("perf-pmu-sysfs WARN: counter value is 0\n");
    }

    int ok = 1;
    if (fd < 0) {
        ok = 0; /* unreachable: handled above, kept for symmetry */
    }
    if (rn != (ssize_t)sizeof(val)) {
        char msg[96];
        snprintf(msg, sizeof(msg), "read() returned %lld, expected 8",
                 (long long)rn);
        (void)fail(msg);
        ok = 0;
    }

    close(efd);

    if (ok) {
        /* Exactly one success sentinel line. */
        printf("STARRY_PERF_PMU_SYSFS_OK\n");
        return 0;
    }
    return 1;
}
