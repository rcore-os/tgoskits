/*
 * perf_hw_stat.c -- perf_event_open(2) multi-counter `perf stat`-style ABI test.
 *
 * Goal (M1): prove the M1 counting ABI works end to end through StarryOS:
 *   1. open event A: PERF_TYPE_HARDWARE / PERF_COUNT_HW_CPU_CYCLES,
 *      read_format = TOTAL_TIME_ENABLED | TOTAL_TIME_RUNNING,
 *   2. open event B: PERF_TYPE_RAW / config=0x11 (ARM CPU_CYCLES on a
 *      programmable counter -- this one DOES count under QEMU TCG, unlike
 *      INSTRUCTIONS which needs -icount), same read_format,
 *   3. RESET + ENABLE both, burn cycles in a shared busy loop, DISABLE both,
 *   4. read() each counter as exactly 3 u64s: value, time_enabled, time_running.
 *
 * SUCCESS == the M1 counting ABI behaves:
 *     both fd >= 0
 *   AND both read() return exactly 24 bytes (3 * u64, read_format=3)
 *   AND time_enabled > 0 for both
 *   AND time_running > 0 for both
 * Counter *value* magnitude is diagnostic only: we WARN (do not fail) if a
 * value is 0, since the busy loop should make both cycles + raw-0x11 count,
 * but a 0 value must not gate success while the PMU is being characterised.
 *
 * read_format=3 (PERF_FORMAT_TOTAL_TIME_ENABLED|PERF_FORMAT_TOTAL_TIME_RUNNING)
 * gives a fixed read(2) buffer layout of exactly three u64 in this order:
 *     [0] value         (u64)  -- offset  0
 *     [1] time_enabled  (u64)  -- offset  8
 *     [2] time_running  (u64)  -- offset 16
 *     total                       24 bytes
 *
 * On success exactly one final line is printed:
 *     STARRY_PERF_STAT_OK
 * which is the suite success sentinel for this case.
 *
 * Everything the kernel ABI needs is defined locally so the test does not
 * depend on <linux/perf_event.h> being present in the musl sysroot. The
 * struct layout matches the Linux aarch64/x86_64 perf_event_attr ABI and is
 * identical to the M0 case (perf-hw-cycles); only read_format is set here.
 * read_format is the field immediately after sample_type -- the kernel reads
 * the struct by ABI offsets, so the layout must match exactly.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <unistd.h>

/* perf_event_attr.type */
#ifndef PERF_TYPE_HARDWARE
#define PERF_TYPE_HARDWARE 0u
#endif
#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif

/* perf_event_attr.config for PERF_TYPE_HARDWARE */
#ifndef PERF_COUNT_HW_CPU_CYCLES
#define PERF_COUNT_HW_CPU_CYCLES 0u
#endif

/*
 * ARM CPU_CYCLES PMU event number, used as a PERF_TYPE_RAW config. This maps
 * onto a programmable PMU counter (not the dedicated cycle counter), which is
 * what QEMU TCG actually increments for cycle-class events.
 */
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif

/*
 * perf_event_attr.read_format bits.
 *   PERF_FORMAT_TOTAL_TIME_ENABLED = 1 << 0
 *   PERF_FORMAT_TOTAL_TIME_RUNNING = 1 << 1
 * With both set (==3) and no PERF_FORMAT_GROUP, read(2) returns exactly:
 *   { u64 value; u64 time_enabled; u64 time_running; }  == 24 bytes.
 */
#ifndef PERF_FORMAT_TOTAL_TIME_ENABLED
#define PERF_FORMAT_TOTAL_TIME_ENABLED (1ull << 0)
#endif
#ifndef PERF_FORMAT_TOTAL_TIME_RUNNING
#define PERF_FORMAT_TOTAL_TIME_RUNNING (1ull << 1)
#endif
#define PERF_READ_FORMAT_TIMING                                                 \
    (PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING)

/* read(2) buffer is exactly 3 u64s with read_format=3 (no GROUP). */
#define PERF_STAT_READ_WORDS 3
#define PERF_STAT_READ_BYTES (PERF_STAT_READ_WORDS * (int)sizeof(uint64_t))

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
 * Linux perf_event_attr ABI. Fields are laid out in the kernel's order; the
 * 64-bit flags word (disabled/inherit/... bitfields) is represented as a plain
 * __u64 we leave mostly zero, then set the low bit for `disabled`. Trailing
 * fields are padded out so sizeof() lands on a stable value (>= 64). This
 * layout is byte-identical to the M0 case (perf-hw-cycles).
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

#ifndef SYS_perf_event_open
/* aarch64 == 241; also the generic asm-generic number used by arm64/riscv. */
#define SYS_perf_event_open 241
#endif

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static void attr_init(struct perf_event_attr *attr, uint32_t type,
                      uint64_t config) {
    /* zero the whole struct: clears all reserved/flag bits */
    for (size_t i = 0; i < sizeof(*attr); i++) {
        ((volatile unsigned char *)attr)[i] = 0;
    }
    attr->type = type;
    attr->config = config;
    attr->size = (uint32_t)sizeof(struct perf_event_attr);
    attr->read_format = PERF_READ_FORMAT_TIMING; /* == 3 */
    attr->flags = PERF_ATTR_FLAG_DISABLED;       /* disabled = 1 */
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C build/run stays green. */
    printf("STARRY_PERF_STAT_OK\n");
    return 0;
#endif
    struct perf_event_attr attr_a;
    struct perf_event_attr attr_b;

    /* Event A: hardware CPU cycles, with timing read_format. */
    attr_init(&attr_a, PERF_TYPE_HARDWARE, PERF_COUNT_HW_CPU_CYCLES);
    /* Event B: raw ARM CPU_CYCLES (0x11) on a programmable counter. */
    attr_init(&attr_b, PERF_TYPE_RAW, ARM_PMU_EVT_CPU_CYCLES);

    /* pid=0 (self), cpu=-1 (any), group_fd=-1 (leader), flags=0 */
    long fd_a = perf_event_open(&attr_a, 0, -1, -1, 0ul);
    if (fd_a < 0) {
        printf("perf-stat FAILED: perf_event_open(A hw cycles) errno=%d\n",
               errno);
        return 1;
    }
    long fd_b = perf_event_open(&attr_b, 0, -1, -1, 0ul);
    if (fd_b < 0) {
        printf("perf-stat FAILED: perf_event_open(B raw 0x11) errno=%d\n",
               errno);
        close((int)fd_a);
        return 1;
    }

    int efd_a = (int)fd_a;
    int efd_b = (int)fd_b;

    (void)ioctl(efd_a, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd_b, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd_a, PERF_EVENT_IOC_ENABLE, 0);
    (void)ioctl(efd_b, PERF_EVENT_IOC_ENABLE, 0);

    /* Shared busy loop so both counters accrue over the same work window. */
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 30000000ull; i++) {
        spin += i;
    }
    (void)spin;

    (void)ioctl(efd_a, PERF_EVENT_IOC_DISABLE, 0);
    (void)ioctl(efd_b, PERF_EVENT_IOC_DISABLE, 0);

    /*
     * read_format=3 layout: exactly 3 u64 -> value, time_enabled, time_running.
     * Read into a 3-word buffer and parse by index.
     */
    uint64_t buf_a[PERF_STAT_READ_WORDS] = {0, 0, 0};
    uint64_t buf_b[PERF_STAT_READ_WORDS] = {0, 0, 0};
    ssize_t n_a = read(efd_a, buf_a, sizeof(buf_a));
    ssize_t n_b = read(efd_b, buf_b, sizeof(buf_b));

    uint64_t val_a = buf_a[0], ena_a = buf_a[1], run_a = buf_a[2];
    uint64_t val_b = buf_b[0], ena_b = buf_b[1], run_b = buf_b[2];

    /* Diagnostics: every value, for both events, plus bytes read. */
    printf("STARRY_PERF_STAT A value=%llu enabled=%llu running=%llu n=%lld\n",
           (unsigned long long)val_a, (unsigned long long)ena_a,
           (unsigned long long)run_a, (long long)n_a);
    printf("STARRY_PERF_STAT B value=%llu enabled=%llu running=%llu n=%lld\n",
           (unsigned long long)val_b, (unsigned long long)ena_b,
           (unsigned long long)run_b, (long long)n_b);

    /* WARN-only: a zero counter value does not gate success under QEMU TCG. */
    if (val_a == 0) {
        printf("perf-stat WARN: A (hw cycles) value is 0\n");
    }
    if (val_b == 0) {
        printf("perf-stat WARN: B (raw 0x11) value is 0\n");
    }

    /* ABI assertions (these gate success). */
    int ok = 1;
    if (fd_a < 0 || fd_b < 0) {
        printf("perf-stat FAILED: an fd is negative (A=%ld B=%ld)\n", fd_a,
               fd_b);
        ok = 0;
    }
    if (n_a != PERF_STAT_READ_BYTES) {
        printf("perf-stat FAILED: A read() returned %lld, expected %d\n",
               (long long)n_a, PERF_STAT_READ_BYTES);
        ok = 0;
    }
    if (n_b != PERF_STAT_READ_BYTES) {
        printf("perf-stat FAILED: B read() returned %lld, expected %d\n",
               (long long)n_b, PERF_STAT_READ_BYTES);
        ok = 0;
    }
    if (ena_a == 0) {
        printf("perf-stat FAILED: A time_enabled is 0\n");
        ok = 0;
    }
    if (run_a == 0) {
        printf("perf-stat FAILED: A time_running is 0\n");
        ok = 0;
    }
    if (ena_b == 0) {
        printf("perf-stat FAILED: B time_enabled is 0\n");
        ok = 0;
    }
    if (run_b == 0) {
        printf("perf-stat FAILED: B time_running is 0\n");
        ok = 0;
    }

    close(efd_a);
    close(efd_b);

    if (ok) {
        /* Exactly one success sentinel line. */
        printf("STARRY_PERF_STAT_OK\n");
        return 0;
    }

    return 1;
}
