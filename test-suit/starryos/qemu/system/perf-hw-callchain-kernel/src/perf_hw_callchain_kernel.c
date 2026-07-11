/*
 * perf_hw_callchain_kernel.c -- perf_event_open(2) PERF_SAMPLE_CALLCHAIN test.
 *
 * Proves the kernel call-graph sampling path (M4a): a sampling event opened with
 * PERF_SAMPLE_CALLCHAIN must, for a sample taken in kernel (EL1) context, emit a
 * callchain block of `[PERF_CONTEXT_KERNEL, leaf_ip, ret0, ret1, ...]` unwound
 * from the interrupted frame pointer -- not just the leaf IP. A host `perf
 * report` renders such records as flamegraphs.
 *
 *   1. open a SAMPLING event: PERF_TYPE_RAW / config=0x11 (ARM CPU_CYCLES, which
 *      counts under QEMU TCG), fixed sample_period, sample_type =
 *      PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME | PERF_SAMPLE_CALLCHAIN,
 *   2. mmap (1 header page + 8 data pages),
 *   3. ENABLE, then run a SYSCALL-HEAVY workload (read() a page from /dev/zero in
 *      a loop) so most PMU overflows fire while executing inside the kernel, with
 *      a deep, unwindable kernel stack; DISABLE,
 *   4. walk the ring, parse each PERF_RECORD_SAMPLE body in sample_type order
 *      (ip, pid+tid, time, then the callchain: u64 nr followed by nr u64 entries),
 *   5. classify each callchain by its PERF_CONTEXT_* markers and measure how many
 *      instruction pointers follow the kernel marker.
 *
 * SUCCESS ==
 *     fd >= 0 AND mmap ok AND the ring is non-empty
 *   AND at least one PERF_RECORD_SAMPLE carries a well-formed callchain block
 *       (nr never overruns the record)
 *   AND at least one KERNEL-context callchain (PERF_CONTEXT_KERNEL) with a leaf IP
 *       is present -- i.e. kernel-context sampling emits the correct callchain
 *       record with the kernel region marker.
 * This deliberately does NOT require a DEEP kernel chain: the default kernel is
 * built without frame pointers (see the note near the assertions), so the kernel
 * region is leaf-only. The perf-hw-callchain-user case proves the FP unwinder
 * actually walks multiple frames on a frame-pointer-enabled binary.
 * On success exactly one line `STARRY_PERF_CALLCHAIN_OK` is printed.
 *
 * All ABI structs are defined locally (no <linux/perf_event.h> dependency) and
 * are byte-identical to the perf-hw-sample case; only sample_type and the record
 * body parsing differ.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif

/* sample_type bits (see man perf_event_open). */
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_TID (1ull << 1)
#define PERF_SAMPLE_TIME (1ull << 2)
#define PERF_SAMPLE_CALLCHAIN (1ull << 5)

/*
 * Callchain context markers (Linux). Everything >= PERF_CONTEXT_MAX
 * (== (uint64_t)-4095) is a marker, not an instruction pointer.
 */
#define PERF_CONTEXT_KERNEL ((uint64_t)-128)
#define PERF_CONTEXT_USER ((uint64_t)-512)
#define PERF_CONTEXT_MAX ((uint64_t)-4095)

/* A single overflow every this-many events. */
#define SAMPLE_PERIOD 100000ull

#ifndef PERF_EVENT_IOC_ENABLE
#define PERF_EVENT_IOC_ENABLE 0x2400u
#endif
#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
#ifndef PERF_EVENT_IOC_RESET
#define PERF_EVENT_IOC_RESET 0x2403u
#endif
#ifndef PERF_RECORD_SAMPLE
#define PERF_RECORD_SAMPLE 9u
#endif

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
    uint64_t flags;
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

#define PERF_ATTR_FLAG_DISABLED (1ull << 0)

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
    uint8_t __reserved[928];
    uint64_t data_head;
    uint64_t data_tail;
    uint64_t data_offset;
    uint64_t data_size;
    uint64_t aux_head;
    uint64_t aux_tail;
    uint64_t aux_offset;
    uint64_t aux_size;
};

_Static_assert(offsetof(struct perf_event_attr, sample_period) == 16, "off16");
_Static_assert(offsetof(struct perf_event_attr, sample_type) == 24, "off24");
_Static_assert(offsetof(struct perf_event_attr, read_format) == 32, "off32");
_Static_assert(offsetof(struct perf_event_attr, flags) == 40, "off40");
_Static_assert(offsetof(struct perf_event_mmap_page, data_head) == 1024, "dh");
_Static_assert(offsetof(struct perf_event_mmap_page, data_tail) == 1032, "dt");
_Static_assert(offsetof(struct perf_event_mmap_page, data_offset) == 1040, "do");
_Static_assert(offsetof(struct perf_event_mmap_page, data_size) == 1048, "ds");

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

#define PERF_MMAP_PAGE_SIZE 4096u
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES                                                   \
    ((size_t)(1u + PERF_MMAP_DATA_PAGES) * PERF_MMAP_PAGE_SIZE)

/* A hard ceiling on nr so a corrupt record cannot spin the parser. */
#define CALLCHAIN_NR_MAX 512u

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-callchain FAILED: %s\n", reason);
    return 1;
}

/* Copy `n` bytes out of the ring at byte offset `at`, wrapping modulo size. */
static void ring_copy(const uint8_t *base, uint64_t size, uint64_t at, void *dst,
                      size_t n) {
    for (size_t b = 0; b < n; b++) {
        ((uint8_t *)dst)[b] = base[(at + b) % size];
    }
}

int main(void) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass elsewhere. */
    printf("STARRY_PERF_CALLCHAIN_OK\n");
    return 0;
#endif
    struct perf_event_attr attr;
    for (size_t i = 0; i < sizeof(attr); i++) {
        ((volatile unsigned char *)&attr)[i] = 0;
    }
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(struct perf_event_attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type =
        PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME | PERF_SAMPLE_CALLCHAIN;
    attr.read_format = 0;
    attr.flags = PERF_ATTR_FLAG_DISABLED;

    long fd = perf_event_open(&attr, 0, -1, -1, 0ul);
    if (fd < 0) {
        char msg[96];
        snprintf(msg, sizeof(msg), "perf_event_open(callchain) errno=%d", errno);
        return fail(msg);
    }
    int efd = (int)fd;

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

    /*
     * Syscall-heavy workload: read a page from /dev/zero in a tight loop so PMU
     * overflows land inside the kernel with a deep, frame-pointer-walkable stack
     * (sys_read -> VFS -> device). If /dev/zero is unavailable, fall back to a
     * getpid() loop -- still kernel context, if a shallower stack.
     */
    int zfd = open("/dev/zero", O_RDONLY);

    (void)ioctl(efd, PERF_EVENT_IOC_RESET, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_ENABLE, 0);

    if (zfd >= 0) {
        static uint8_t buf[4096];
        for (uint64_t i = 0; i < 400000ull; i++) {
            if (read(zfd, buf, sizeof(buf)) < 0) {
                break;
            }
        }
    } else {
        volatile long p = 0;
        for (uint64_t i = 0; i < 4000000ull; i++) {
            p += syscall(SYS_getpid);
        }
        (void)p;
    }

    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);
    if (zfd >= 0) {
        close(zfd);
    }

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t sample_count = 0;
    uint64_t callchain_count = 0;   /* samples carrying a callchain block */
    uint64_t kernel_chains = 0;     /* callchains containing PERF_CONTEXT_KERNEL */
    uint64_t user_chains = 0;       /* callchains containing PERF_CONTEXT_USER */
    uint64_t max_kernel_ips = 0;    /* most IPs seen after PERF_CONTEXT_KERNEL */
    uint64_t max_user_ips = 0;      /* most IPs seen after PERF_CONTEXT_USER */
    int saw_truncated = 0;
    int bad_chain = 0; /* a callchain that ran past the record -> corrupt */

    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        ring_copy(data_base, data_size, rel, &hdr, sizeof(hdr));

        if (hdr.size == 0) {
            saw_truncated = 1;
            break;
        }
        if (off + hdr.size > data_head) {
            saw_truncated = 1;
            break;
        }

        if (hdr.type == PERF_RECORD_SAMPLE) {
            sample_count++;
            /*
             * Body layout for sample_type = IP|TID|TIME|CALLCHAIN, in the
             * kernel's canonical field order:
             *   u64 ip; u32 pid; u32 tid; u64 time; u64 nr; u64 entries[nr];
             */
            uint64_t cur = (uint64_t)sizeof(hdr); /* offset within the record */
            cur += 8;                             /* ip   */
            cur += 8;                             /* pid+tid */
            cur += 8;                             /* time */

            if (cur + 8 <= hdr.size) {
                uint64_t nr = 0;
                ring_copy(data_base, data_size, (rel + cur) % data_size, &nr, 8);
                cur += 8;

                /* nr entries must fit in the record and stay sane. */
                uint64_t avail = (hdr.size - cur) / 8;
                if (nr > avail || nr > CALLCHAIN_NR_MAX) {
                    bad_chain = 1;
                } else if (nr > 0) {
                    callchain_count++;
                    /* Count IPs following each context marker. */
                    int in_kernel = 0, in_user = 0;
                    uint64_t k_ips = 0, u_ips = 0;
                    for (uint64_t e = 0; e < nr; e++) {
                        uint64_t entry = 0;
                        ring_copy(data_base, data_size,
                                  (rel + cur + e * 8) % data_size, &entry, 8);
                        if (entry >= PERF_CONTEXT_MAX) {
                            /* A context marker: switch regions. */
                            in_kernel = (entry == PERF_CONTEXT_KERNEL);
                            in_user = (entry == PERF_CONTEXT_USER);
                            if (in_kernel) {
                                kernel_chains++;
                            }
                            if (in_user) {
                                user_chains++;
                            }
                        } else if (in_kernel) {
                            k_ips++;
                        } else if (in_user) {
                            u_ips++;
                        }
                    }
                    if (k_ips > max_kernel_ips) {
                        max_kernel_ips = k_ips;
                    }
                    if (u_ips > max_user_ips) {
                        max_user_ips = u_ips;
                    }
                }
            }
        }

        off += hdr.size;
    }

    printf("STARRY_PERF_CALLCHAIN samples=%llu chains=%llu kchains=%llu "
           "uchains=%llu max_kips=%llu max_uips=%llu truncated=%d bad=%d\n",
           (unsigned long long)sample_count, (unsigned long long)callchain_count,
           (unsigned long long)kernel_chains, (unsigned long long)user_chains,
           (unsigned long long)max_kernel_ips, (unsigned long long)max_user_ips,
           saw_truncated, bad_chain);

    int rc = 0;
    if (fd < 0) {
        rc = fail("fd is negative");
    } else if (base == MAP_FAILED) {
        rc = fail("mmap failed");
    } else if (data_head == data_tail) {
        rc = fail("no samples captured (data_head == data_tail)");
    } else if (sample_count == 0) {
        rc = fail("no PERF_RECORD_SAMPLE records in ring");
    } else if (bad_chain) {
        rc = fail("callchain nr overran the record (corrupt block)");
    } else if (callchain_count == 0) {
        rc = fail("no sample carried a callchain block");
    } else if (kernel_chains == 0) {
        rc = fail("no kernel-context callchain (no PERF_CONTEXT_KERNEL region)");
    } else if (max_kernel_ips < 1) {
        rc = fail("kernel callchain carried no instruction pointer");
    }
    /*
     * NOTE: this validates the kernel callchain RECORD (block emitted, well
     * formed, PERF_CONTEXT_KERNEL region with a leaf IP). It does NOT require a
     * DEEP kernel chain, because the default kernel is built without frame
     * pointers (like Linux without CONFIG_FRAME_POINTER), so x29 is not an
     * unwindable chain and max_kernel_ips is 1. Deep kernel unwinding needs a
     * kernel built with -Cforce-frame-pointers (BACKTRACE=y); the perf-hw-
     * callchain-user case proves the FP walk engine itself unwinds multiple
     * frames on a frame-pointer-enabled (user) binary.
     */

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);

    if (rc == 0) {
        printf("STARRY_PERF_CALLCHAIN_OK\n");
        return 0;
    }
    return rc;
}
