/*
 * perf_hw_sample.c -- perf_event_open(2) `perf record`-style SAMPLING ABI test.
 *
 * Goal (M2): prove the M2 sampling path works end to end through StarryOS, i.e.
 * that a counter configured with a sample_period overflows, the kernel writes
 * PERF_RECORD_SAMPLE records into the mmap ring, and userspace can read them:
 *   1. open a SAMPLING event: PERF_TYPE_RAW / config=0x11 (ARM CPU_CYCLES -- a
 *      programmable counter that DOES count under QEMU TCG), sample_period set
 *      (freq bit OFF => it is a period), sample_type = PERF_SAMPLE_IP,
 *      read_format = 0, disabled = 1,
 *   2. mmap (1 header page + 8 data pages) with PROT_READ|PROT_WRITE/MAP_SHARED,
 *   3. ENABLE, burn a large busy loop so the counter overflows MANY times at
 *      period 100000, DISABLE,
 *   4. read the ring control header (data_head/data_tail/data_offset/data_size)
 *      from the first mmap page (struct perf_event_mmap_page),
 *   5. walk the records from data_tail to data_head, parsing each
 *      perf_event_header; for PERF_RECORD_SAMPLE (type 9) with
 *      sample_type = PERF_SAMPLE_IP the body is a single u64 ip.
 *
 * SUCCESS == the M2 sampling ABI behaves:
 *     fd >= 0
 *   AND mmap() succeeded (!= MAP_FAILED)
 *   AND data_head != data_tail (the ring is non-empty -- samples were captured)
 *   AND at least one record with type == PERF_RECORD_SAMPLE (9) is present
 *   AND the sampled ip is non-zero.
 *
 * On success exactly one final line is printed:
 *     STARRY_PERF_SAMPLE_OK
 * which is the suite success sentinel for this case. On failure a line
 * "perf-sample FAILED: <reason>" is printed and the process exits non-zero,
 * which the grouped runner treats as a failure.
 *
 * Everything the kernel ABI needs is defined locally so the test does not
 * depend on <linux/perf_event.h> being present in the musl sysroot.
 *
 * perf_event_attr is byte-identical to the M0/M1 cases (perf-hw-cycles /
 * perf-hw-stat): sample_period is the union at offset 16 (right after config),
 * sample_type at offset 24, read_format at offset 32, flags (disabled bit) at
 * offset 40. The kernel reads the struct by ABI offset, so the layout must
 * match exactly; only the field VALUES differ from M1 here.
 *
 * perf_event_mmap_page matches the Linux ABI: the ring control words live in
 * the second cache-line region at fixed offsets -- data_head @ 1024,
 * data_tail @ 1032, data_offset @ 1040, data_size @ 1048 -- regardless of how
 * many of the leading timing/capability fields a given kernel populates.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

/* perf_event_attr.type */
#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif

/*
 * ARM CPU_CYCLES PMU event number, used as a PERF_TYPE_RAW config. This maps
 * onto a programmable PMU counter (not the dedicated cycle counter), which is
 * what QEMU TCG actually increments for cycle-class events -- so the counter
 * reaches the sample_period and overflows under emulation.
 */
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif

/*
 * perf_event_attr.sample_type bits. PERF_SAMPLE_IP == 1 << 0: each
 * PERF_RECORD_SAMPLE body is a single u64 instruction pointer.
 */
#ifndef PERF_SAMPLE_IP
#define PERF_SAMPLE_IP (1ull << 0)
#endif

/* Overflow period: the counter raises a sample every this-many events. */
#define SAMPLE_PERIOD 100000ull

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

/* perf_event_header.type for a sample record. */
#ifndef PERF_RECORD_SAMPLE
#define PERF_RECORD_SAMPLE 9u
#endif

/*
 * Linux perf_event_attr ABI -- byte-identical to the M0/M1 cases. Fields are
 * laid out in the kernel's order; the 64-bit flags word (disabled/freq/...
 * bitfields) is a plain __u64 we leave mostly zero, then set the low bit for
 * `disabled`. With the `freq` bit (bit 10) left zero the sample_period union is
 * interpreted as a fixed PERIOD, which is what we want.
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

    uint64_t flags; /* bit 0 == disabled; bit 10 == freq (left zero) */

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

/*
 * Linux perf_event_mmap_page -- the first mmap page. We only consume the ring
 * control words, but the leading fields are declared so the offsets are exact.
 * The kernel places data_head/data_tail/data_offset/data_size at fixed offsets
 * 1024/1032/1040/1048; __reserved pads the named block up to 1024.
 */
struct perf_event_mmap_page {
    uint32_t version;
    uint32_t compat_version;
    uint32_t lock;
    uint32_t index;
    int64_t offset;
    uint64_t time_enabled;
    uint64_t time_running;
    union {
        uint64_t capabilities;
        struct {
            uint64_t cap_bit0 : 1, cap_bit0_is_deprecated : 1,
                cap_user_rdpmc : 1, cap_user_time : 1, cap_user_time_zero : 1,
                cap_user_time_short : 1, cap_____res : 58;
        };
    };
    uint16_t pmc_width;
    uint16_t time_shift;
    uint32_t time_mult;
    uint64_t time_offset;
    uint64_t time_zero;
    uint32_t size;
    uint32_t __reserved_1;
    uint64_t time_cycles;
    uint64_t time_mask;
    uint8_t __reserved[928]; /* pad named block up to offset 1024 */
    uint64_t data_head;      /* @ 1024 -- head of the data section */
    uint64_t data_tail;      /* @ 1032 -- tail (consumed up to here) */
    uint64_t data_offset;    /* @ 1040 -- byte offset of the data section */
    uint64_t data_size;      /* @ 1048 -- size of the data section */
    uint64_t aux_head;
    uint64_t aux_tail;
    uint64_t aux_offset;
    uint64_t aux_size;
};

/* Compile-time guards: the ABI offsets that make this test correct. */
_Static_assert(offsetof(struct perf_event_attr, sample_period) == 16,
               "perf_event_attr.sample_period must be at offset 16");
_Static_assert(offsetof(struct perf_event_attr, sample_type) == 24,
               "perf_event_attr.sample_type must be at offset 24");
_Static_assert(offsetof(struct perf_event_attr, read_format) == 32,
               "perf_event_attr.read_format must be at offset 32");
_Static_assert(offsetof(struct perf_event_attr, flags) == 40,
               "perf_event_attr.flags (disabled bit) must be at offset 40");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024,
               "perf_event_mmap_page.data_head must be at offset 1024");
_Static_assert(offsetof(struct perf_event_mmap_page, data_tail) == 1032,
               "perf_event_mmap_page.data_tail must be at offset 1032");
_Static_assert(offsetof(struct perf_event_mmap_page, data_offset) == 1040,
               "perf_event_mmap_page.data_offset must be at offset 1040");
_Static_assert(offsetof(struct perf_event_mmap_page, data_size) == 1048,
               "perf_event_mmap_page.data_size must be at offset 1048");

/* Each ring record starts with this header. */
struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
/* aarch64 == 241; also the generic asm-generic number used by arm64/riscv. */
#define SYS_perf_event_open 241
#endif

/* mmap geometry: 1 header page + 8 data pages (data pages must be 2^n). */
#define PERF_MMAP_PAGE_SIZE 4096u
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES                                                   \
    ((size_t)(1u + PERF_MMAP_DATA_PAGES) * PERF_MMAP_PAGE_SIZE)

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-sample FAILED: %s\n", reason);
    return 1;
}

int main(void) {
    struct perf_event_attr attr;
    /* zero the whole struct: clears all reserved/flag bits */
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }

    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES; /* 0x11 -- counts under QEMU TCG */
    attr.size = (uint32_t)sizeof(struct perf_event_attr);
    attr.sample_period = SAMPLE_PERIOD; /* freq bit off => fixed period */
    attr.sample_type = PERF_SAMPLE_IP;  /* sample body is a single u64 ip */
    attr.read_format = 0;
    attr.flags = PERF_ATTR_FLAG_DISABLED; /* disabled = 1 */

    /* pid=0 (self), cpu=-1 (any), group_fd=-1 (leader), flags=0 */
    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open(sampling) errno=%d", errno);
        return fail(msg);
    }
    int efd = (int)fd;

    /* mmap the ring: 1 header page + 8 data pages. */
    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        int e = errno;
        char msg[96];
        snprintf(msg, sizeof(msg), "mmap ring errno=%d", e);
        close(efd);
        return fail(msg);
    }

    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    (void)ioctl(efd, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_ENABLE, 0);

    /*
     * Large busy loop so the 0x11 counter crosses SAMPLE_PERIOD many times and
     * the kernel writes many PERF_RECORD_SAMPLE records into the ring.
     */
    volatile uint64_t spin = 0;
    for (uint64_t i = 0; i < 200000000ull; i++) {
        spin += i;
    }

    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    /*
     * Read the ring control header. data_head is written by the kernel; pair
     * the load with a full barrier so we observe all record bytes the kernel
     * stored before advancing data_head.
     */
    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;

    /* The data section starts at base + data_offset and is data_size bytes. */
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t sample_count = 0;
    uint64_t first_ip = 0;
    int saw_truncated = 0;

    /*
     * Walk records from data_tail up to data_head. Ring offsets wrap modulo
     * data_size; each record's header.size is the full record length.
     */
    uint64_t off = data_tail;
    while (off < data_head) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;

        /* The header may straddle the wrap boundary; copy it byte-wise. */
        for (size_t b = 0; b < sizeof(hdr); b++) {
            ((uint8_t *)&hdr)[b] = data_base[(rel + b) % data_size];
        }

        if (hdr.size == 0) {
            /* Defensive: a zero-size record would loop forever. */
            saw_truncated = 1;
            break;
        }
        if (off + hdr.size > data_head) {
            /* Partial record at the head -- stop. */
            saw_truncated = 1;
            break;
        }

        if (hdr.type == PERF_RECORD_SAMPLE) {
            /*
             * sample_type == PERF_SAMPLE_IP => body is one u64 ip immediately
             * after the 8-byte header (record size should be 16). Copy the ip
             * byte-wise to tolerate a wrap inside the record.
             */
            uint64_t ip = 0;
            uint64_t body = rel + sizeof(hdr);
            for (size_t b = 0; b < sizeof(ip); b++) {
                ((uint8_t *)&ip)[b] = data_base[(body + b) % data_size];
            }
            if (sample_count == 0) {
                first_ip = ip;
            }
            sample_count++;
        }

        off += hdr.size;
    }

    /* Diagnostics. */
    printf("STARRY_PERF_SAMPLE count=%llu first_ip=0x%llx data_head=%llu "
           "data_tail=%llu data_offset=%llu data_size=%llu truncated=%d\n",
           (unsigned long long)sample_count, (unsigned long long)first_ip,
           (unsigned long long)data_head, (unsigned long long)data_tail,
           (unsigned long long)data_offset, (unsigned long long)data_size,
           saw_truncated);

    /* Assertions that gate success. */
    int rc = 0;
    if (fd < 0) {
        rc = fail("fd is negative");
    } else if (base == MAP_FAILED) {
        rc = fail("mmap failed");
    } else if (data_head == data_tail) {
        rc = fail("no samples captured (data_head == data_tail)");
    } else if (sample_count == 0) {
        rc = fail("no PERF_RECORD_SAMPLE records in ring");
    } else if (first_ip == 0) {
        rc = fail("sampled ip is zero");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);

    if (rc == 0) {
        /* Exactly one success sentinel line. */
        printf("STARRY_PERF_SAMPLE_OK\n");
        return 0;
    }
    return rc;
}
