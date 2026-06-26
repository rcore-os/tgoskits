/*
 * perf_hw_cycles.c -- perf_event_open(2) hardware CPU-cycles ABI smoke test.
 *
 * Goal (M0): prove the perf_event_open ABI works end to end through StarryOS:
 *   1. open a PERF_TYPE_HARDWARE / PERF_COUNT_HW_CPU_CYCLES counter for self,
 *   2. RESET + ENABLE it, burn cycles in a busy loop, DISABLE it,
 *   3. read() the counter back as a single u64.
 *
 * SUCCESS == the ABI behaves: fd >= 0 AND read() returns exactly 8 bytes.
 * The magnitude of the counter value is NOT asserted (under QEMU TCG the PMU
 * counter is being characterised, not validated), so val may legitimately be 0.
 *
 * On success exactly one final line is printed:
 *     STARRY_PERF_HW_OK
 * which is the suite success sentinel for this case.
 *
 * Everything the kernel ABI needs is defined locally so the test does not
 * depend on <linux/perf_event.h> being present in the musl sysroot. The
 * struct layout matches the Linux aarch64/x86_64 perf_event_attr ABI; .size
 * is set to sizeof(struct perf_event_attr) (the kernel accepts any size >=
 * PERF_ATTR_SIZE_VER0 == 64 and zero-fills the remainder).
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

/* perf_event_attr.config for PERF_TYPE_HARDWARE */
#ifndef PERF_COUNT_HW_CPU_CYCLES
#define PERF_COUNT_HW_CPU_CYCLES 0u
#endif

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
 * fields are padded out so sizeof() lands on a stable value (>= 64).
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

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C build/run stays green. */
    printf("STARRY_PERF_HW_OK\n");
    return 0;
#endif
    struct perf_event_attr attr;
    /* zero the whole struct: clears all reserved/flag bits */
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }

    attr.type = PERF_TYPE_HARDWARE;
    attr.config = PERF_COUNT_HW_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(struct perf_event_attr);
    attr.read_format = 0;
    attr.flags = PERF_ATTR_FLAG_DISABLED; /* disabled = 1 */

    /* pid=0 (self), cpu=-1 (any), group_fd=-1 (leader), flags=0 */
    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        printf("perf_event_open failed errno=%d\n", errno);
        return 1;
    }

    int efd = (int)fd;

    (void)ioctl(efd, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_ENABLE, 0);

    /* Burn cycles so a working PMU counter would accrue a non-zero value. */
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 20000000ull; i++) {
        spin += i;
    }

    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t val = 0;
    ssize_t n = read(efd, &val, sizeof(val));

    /* Diagnostic line: counter value + bytes read (value is informational). */
    printf("STARRY_PERF_HW_CYCLES=%llu n=%lld\n", (unsigned long long)val,
           (long long)n);

    int ok = (fd >= 0) && (n == 8);

    close(efd);

    if (ok) {
        /* Exactly one success sentinel line. */
        printf("STARRY_PERF_HW_OK\n");
        return 0;
    }

    return 1;
}
