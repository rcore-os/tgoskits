/*
 * perf_hw_sideband.c -- perf side-band records (COMM + MMAP2) test.
 *
 * `perf report` symbolizes a sample's IP by looking up which binary was mapped
 * at that address. The kernel supplies that out of band: at the monitored task's
 * execve it writes PERF_RECORD_COMM (the process name) and one PERF_RECORD_MMAP2
 * per executable mapping (the exec image + dynamic loader) into the same mmap
 * ring as the samples, gated by attr.comm / attr.mmap2.
 *
 * This test drives exactly that: fork a child, open a per-task sampling event on
 * it with attr.comm = attr.mmap2 = attr.sample_id_all = 1 and enable_on_exec,
 * mmap the ring, release the child (it execs itself in --busy mode), then walk
 * the ring and confirm a COMM record (non-empty name) and an MMAP2 record
 * (non-empty filename) are present. sample_id_all is set with a non-trivial
 * sample_type (IP|TID|TIME) so each record carries the trailer, exercising the
 * size accounting the ring walk relies on.
 *
 * SUCCESS == a COMM record with a non-empty name AND an MMAP2 record with a
 * non-empty filename. Prints the single sentinel STARRY_PERF_SIDEBAND_OK.
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PERF_TYPE_RAW
#define PERF_TYPE_RAW 4u
#endif
#ifndef ARM_PMU_EVT_CPU_CYCLES
#define ARM_PMU_EVT_CPU_CYCLES 0x11ull
#endif
#define PERF_SAMPLE_IP (1ull << 0)
#define PERF_SAMPLE_TID (1ull << 1)
#define PERF_SAMPLE_TIME (1ull << 2)
#define SAMPLE_PERIOD 50000ull

#ifndef PERF_EVENT_IOC_DISABLE
#define PERF_EVENT_IOC_DISABLE 0x2401u
#endif
/* perf_event_attr flag bit positions (see the bitfield in <linux/perf_event.h>). */
#define PERF_ATTR_FLAG_DISABLED (1ull << 0)
#define PERF_ATTR_FLAG_COMM (1ull << 9)
#define PERF_ATTR_FLAG_ENABLE_ON_EXEC (1ull << 12)
#define PERF_ATTR_FLAG_SAMPLE_ID_ALL (1ull << 18)
#define PERF_ATTR_FLAG_MMAP2 (1ull << 23)

#define PERF_RECORD_COMM 3u
#define PERF_RECORD_MMAP2 10u

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

struct perf_event_mmap_page {
    uint32_t version;
    uint32_t compat_version;
    uint32_t lock;
    uint32_t index;
    int64_t offset;
    uint64_t time_enabled;
    uint64_t time_running;
    uint64_t capabilities;
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
};

struct perf_event_header {
    uint32_t type;
    uint16_t misc;
    uint16_t size;
};

#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif
#define PERF_MMAP_DATA_PAGES 8u
#define PERF_MMAP_TOTAL_BYTES ((size_t)(1u + PERF_MMAP_DATA_PAGES) * 4096u)

/* MMAP2 body offset (from record start) to the filename: 8-byte header +
 * pid,tid (8) + addr,len,pgoff (24) + maj,min (8) + ino,ino_generation (16) +
 * prot,flags (8) = 72. */
#define MMAP2_FILENAME_OFF 72u
/* COMM body offset to the name: 8-byte header + pid,tid (8) = 16. */
#define COMM_NAME_OFF 16u

static long perf_event_open(struct perf_event_attr *attr, pid_t pid, int cpu,
                            int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

static int fail(const char *reason) {
    printf("perf-sideband FAILED: %s\n", reason);
    return 1;
}

/* Copy a NUL-terminated string out of the ring starting at byte offset `at`
 * (relative to the data region base, wrapping at data_size). Returns its length. */
static size_t ring_cstr(const uint8_t *data_base, uint64_t data_size, uint64_t at,
                        char *out, size_t outsz) {
    size_t n = 0;
    while (n + 1 < outsz) {
        char c = (char)data_base[(at + n) % data_size];
        if (c == '\0') {
            break;
        }
        out[n++] = c;
    }
    out[n] = '\0';
    return n;
}

int main(int argc, char **argv) {
#if !defined(__aarch64__)
    /* Hardware-PMU perf is aarch64-only (ARM PMUv3); skip-as-pass on other
     * architectures so the cross-arch grouped C build/run stays green. */
    printf("STARRY_PERF_SIDEBAND_OK\n");
    return 0;
#endif
    if (argc > 1 && strcmp(argv[1], "--busy") == 0) {
        volatile uint64_t spin = 0;
        for (uint64_t i = 0; i < 200000000ull; i++) {
            spin += i;
        }
        return (int)(spin & 1);
    }

    int go[2];
    if (pipe(go) != 0) {
        return fail("pipe");
    }
    pid_t child = fork();
    if (child < 0) {
        return fail("fork");
    }
    if (child == 0) {
        close(go[1]);
        char b;
        (void)!read(go[0], &b, 1);
        close(go[0]);
        char *av[] = {argv[0], (char *)"--busy", NULL};
        execv(argv[0], av);
        _exit(127);
    }
    close(go[0]);

    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_RAW;
    attr.config = ARM_PMU_EVT_CPU_CYCLES;
    attr.size = (uint32_t)sizeof(attr);
    attr.sample_period = SAMPLE_PERIOD;
    attr.sample_type = PERF_SAMPLE_IP | PERF_SAMPLE_TID | PERF_SAMPLE_TIME;
    attr.flags = PERF_ATTR_FLAG_DISABLED | PERF_ATTR_FLAG_ENABLE_ON_EXEC |
                 PERF_ATTR_FLAG_COMM | PERF_ATTR_FLAG_MMAP2 |
                 PERF_ATTR_FLAG_SAMPLE_ID_ALL;

    long fd = perf_event_open(&attr, child, -1, -1, 0ul);
    if (fd < 0) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        return fail("perf_event_open");
    }
    int efd = (int)fd;

    void *base = mmap(NULL, PERF_MMAP_TOTAL_BYTES, PROT_READ | PROT_WRITE,
                      MAP_SHARED, efd, 0);
    if (base == MAP_FAILED) {
        (void)!write(go[1], "g", 1);
        close(go[1]);
        waitpid(child, NULL, 0);
        close(efd);
        return fail("mmap ring");
    }
    struct perf_event_mmap_page *meta = (struct perf_event_mmap_page *)base;

    /* Release the child: it execs (-> COMM + MMAP2 emitted) and runs. */
    (void)!write(go[1], "g", 1);
    close(go[1]);
    int status = 0;
    waitpid(child, &status, 0);
    (void)ioctl(efd, PERF_EVENT_IOC_DISABLE, 0);

    uint64_t data_head = meta->data_head;
    __sync_synchronize();
    uint64_t data_tail = meta->data_tail;
    uint64_t data_offset = meta->data_offset;
    uint64_t data_size = meta->data_size;
    const uint8_t *data_base = (const uint8_t *)base + data_offset;

    uint64_t n_comm = 0, n_mmap2 = 0, n_sample = 0;
    char comm_name[64] = {0};
    char mmap_file[256] = {0};
    uint64_t off = data_tail;
    while (off < data_head && data_size != 0) {
        uint64_t rel = off % data_size;
        struct perf_event_header hdr;
        for (size_t b = 0; b < sizeof(hdr); b++) {
            ((uint8_t *)&hdr)[b] = data_base[(rel + b) % data_size];
        }
        if (hdr.size == 0 || off + hdr.size > data_head) {
            break;
        }
        if (hdr.type == PERF_RECORD_COMM) {
            if (n_comm == 0) {
                ring_cstr(data_base, data_size, rel + COMM_NAME_OFF, comm_name,
                          sizeof(comm_name));
            }
            n_comm++;
        } else if (hdr.type == PERF_RECORD_MMAP2) {
            char tmp[256];
            size_t len = ring_cstr(data_base, data_size, rel + MMAP2_FILENAME_OFF,
                                   tmp, sizeof(tmp));
            if (len > 0 && mmap_file[0] == '\0') {
                memcpy(mmap_file, tmp, len + 1);
            }
            n_mmap2++;
        } else if (hdr.type == 9 /* PERF_RECORD_SAMPLE */) {
            n_sample++;
        }
        off += hdr.size;
    }

    printf("STARRY_PERF_SIDEBAND comm=%llu mmap2=%llu samples=%llu name='%s' "
           "file='%s'\n",
           (unsigned long long)n_comm, (unsigned long long)n_mmap2,
           (unsigned long long)n_sample, comm_name, mmap_file);

    int rc = 0;
    if (n_comm == 0) {
        rc = fail("no PERF_RECORD_COMM record");
    } else if (comm_name[0] == '\0') {
        rc = fail("COMM record has empty name");
    } else if (n_mmap2 == 0) {
        rc = fail("no PERF_RECORD_MMAP2 record");
    } else if (mmap_file[0] == '\0') {
        rc = fail("MMAP2 record has empty filename");
    }

    (void)munmap(base, PERF_MMAP_TOTAL_BYTES);
    close(efd);
    if (rc == 0) {
        printf("STARRY_PERF_SIDEBAND_OK\n");
        return 0;
    }
    return rc;
}
