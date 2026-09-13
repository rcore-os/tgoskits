#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sched.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#if defined(__aarch64__)
struct attr {
    uint32_t type, size;
    uint64_t config, period, sample_type, read_format, flags;
    uint8_t tail[80];
};
struct meta {
    uint8_t reserved[1024];
    uint64_t head, tail, offset, size;
};
struct header { uint32_t type; uint16_t misc, size; };

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + ts.tv_nsec;
}

static void work(void) {
    volatile uint64_t value = 0;
    uint64_t end = now_ns() + 100000000ull;
    do {
        for (unsigned i = 0; i < 1000; ++i) value += i;
    } while (now_ns() < end);
    (void)value;
}

static void copy_ring(const uint8_t *ring, uint64_t size, uint64_t offset,
                      void *dst, unsigned len) {
    for (unsigned i = 0; i < len; ++i)
        ((uint8_t *)dst)[i] = ring[(offset + i) % size];
}

static int check_streams(const char *program) {
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(0, &cpus);
    if (sched_setaffinity(0, sizeof(cpus), &cpus)) return 1;
    struct attr attr = {
        .type = 4, .size = sizeof(attr), .config = 0x11, .period = 1000000,
        .sample_type = 1 | (1ull << 1) | (1ull << 6) | (1ull << 9) | (1ull << 16),
        .flags = (1ull << 1) | (1ull << 9) | (1ull << 18), /* inherit, comm, sample_id_all */
    };
    /* CPU filter makes this inherited task event mmap-able on Linux too. */
    int fd = syscall(SYS_perf_event_open, &attr, 0, 0, -1, 0);
    if (fd < 0) return 1;
    uint64_t root_id = 0;
    if (syscall(SYS_ioctl, fd, 0x80082407u, &root_id)) return 1;
    size_t length = 65 * 4096;
    struct meta *meta = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (meta == MAP_FAILED) return 1;
    pid_t pids[3] = {getpid(), 0, 0};
    work();
    for (unsigned i = 1; i < 3; ++i) {
        pids[i] = fork();
        if (pids[i] < 0) return 1;
        if (!pids[i]) {
            /* Exec emits COMM through the same inherited event instance. */
            execl(program, program, "--child", (char *)NULL);
            _exit(2);
        }
        int status;
        if (waitpid(pids[i], &status, 0) != pids[i] || !WIFEXITED(status) ||
            WEXITSTATUS(status)) return 1;
    }
    if (syscall(SYS_ioctl, fd, 0x2401, 0)) return 1;
    uint64_t head = __atomic_load_n(&meta->head, __ATOMIC_ACQUIRE);
    uint64_t streams[3] = {0}, comm_streams[3] = {0};
    unsigned samples[3] = {0}, failed = 0;
    const uint8_t *ring = (const uint8_t *)meta + meta->offset;
    for (uint64_t at = 0; at < head;) {
        struct header h;
        copy_ring(ring, meta->size, at, &h, sizeof(h));
        if (h.size < sizeof(h) || h.size > head - at) { failed = 1; break; }
        uint64_t words[6] = {0};
        uint64_t identity = 0, id = 0, stream = 0, identifier = 0;
        if (h.type == 9 && h.size == sizeof(words)) {
            copy_ring(ring, meta->size, at, words, sizeof(words));
            identifier = words[1]; identity = words[3]; id = words[4]; stream = words[5];
        } else if (h.type == 3 && h.size >= 48) {
            copy_ring(ring, meta->size, at + h.size - 32, words, 32);
            identity = words[0]; id = words[1]; stream = words[2]; identifier = words[3];
        } else { at += h.size; continue; }
        for (unsigned i = 0; i < 3; ++i) {
            if ((uint32_t)identity != (uint32_t)pids[i]) continue;
            if (id != root_id || identifier != root_id || stream == 0 ||
                (i == 0 ? stream != root_id : stream == root_id)) failed = 1;
            if (h.type == 9) {
                if (streams[i] && streams[i] != stream) failed = 1;
                streams[i] = stream;
                samples[i]++;
            } else comm_streams[i] = stream;
        }
        at += h.size;
    }
    if (!samples[0] || !samples[1] || !samples[2] || streams[1] == streams[2] ||
        comm_streams[1] != streams[1] || comm_streams[2] != streams[2]) failed = 1;
    printf("INHERIT_STREAM root=%llu child=%llu/%llu comm=%llu/%llu samples=%u/%u/%u failed=%u\n",
           (unsigned long long)root_id, (unsigned long long)streams[1],
           (unsigned long long)streams[2], (unsigned long long)comm_streams[1],
           (unsigned long long)comm_streams[2], samples[0], samples[1], samples[2], failed);
    munmap(meta, length);
    close(fd);
    return failed;
}
#endif

int main(int argc, char **argv) {
#if defined(__aarch64__)
    if (argc == 2 && !strcmp(argv[1], "--child")) { work(); return 0; }
    if (check_streams(argv[0])) {
        puts("STARRY_PERF_INHERIT_STREAM_FAILED");
        return 1;
    }
#else
    (void)argc;
    (void)argv;
    puts("SKIP: AArch64 inherited sampling streams");
#endif
    puts("STARRY_PERF_INHERIT_STREAM_OK");
    return 0;
}
