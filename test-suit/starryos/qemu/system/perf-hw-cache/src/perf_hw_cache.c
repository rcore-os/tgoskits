#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#define PERF_TYPE_HW_CACHE 3u
#define CACHE_CFG(id, op, result) ((id) | ((op) << 8) | ((result) << 16))
#ifndef SYS_perf_event_open
#define SYS_perf_event_open 241
#endif

struct perf_event_attr {
    uint32_t type;
    uint32_t size;
    uint64_t config;
    uint64_t sample_period;
    uint64_t sample_type;
    uint64_t read_format;
    uint64_t flags;
    uint8_t tail[80];
};

static int open_cache(uint64_t config) {
    struct perf_event_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.type = PERF_TYPE_HW_CACHE;
    attr.size = sizeof(attr);
    attr.config = config;
    return syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0);
}

int main(void) {
#if !defined(__aarch64__)
    puts("STARRY_PERF_HW_CACHE_OK");
    return 0;
#endif
    static const uint64_t valid[] = {
        CACHE_CFG(0, 0, 0), CACHE_CFG(0, 0, 1), CACHE_CFG(1, 0, 0),
        CACHE_CFG(1, 0, 1), CACHE_CFG(2, 0, 0), CACHE_CFG(2, 0, 1),
        CACHE_CFG(3, 0, 0), CACHE_CFG(3, 0, 1), CACHE_CFG(4, 0, 0),
        CACHE_CFG(4, 0, 1), CACHE_CFG(5, 0, 0), CACHE_CFG(5, 0, 1),
    };
    int accepted = 0;
    for (size_t i = 0; i < sizeof(valid) / sizeof(valid[0]); ++i) {
        int fd = open_cache(valid[i]);
        if (fd >= 0) {
            uint64_t value;
            if (read(fd, &value, sizeof(value)) != (ssize_t)sizeof(value)) {
                return 1;
            }
            close(fd);
            ++accepted;
        } else if (errno != ENOENT && errno != EOPNOTSUPP) {
            printf("perf-hw-cache FAILED: valid[%zu] errno=%d\n", i, errno);
            return 1;
        }
    }

    int fd = open_cache(CACHE_CFG(0, 2, 0));
    if (fd >= 0 || errno != ENOENT) {
        printf("perf-hw-cache FAILED: unsupported errno=%d fd=%d\n", errno, fd);
        return 1;
    }
    fd = open_cache(CACHE_CFG(7, 0, 0));
    if (fd >= 0 || errno != EINVAL) {
        printf("perf-hw-cache FAILED: malformed errno=%d fd=%d\n", errno, fd);
        return 1;
    }
    printf("STARRY_PERF_HW_CACHE_OK accepted=%d\n", accepted);
    return 0;
}
