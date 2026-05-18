#include "helpers.h"
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <time.h>

int create_pipe(int fds[2]) {
    return pipe(fds);
}

int create_nonblocking_pipe(int fds[2]) {
    if (pipe(fds) < 0) return -1;
    fcntl(fds[0], F_SETFL, O_NONBLOCK);
    fcntl(fds[1], F_SETFL, O_NONBLOCK);
    return 0;
}

int write_exact(int fd, const void *buf, size_t count) {
    const char *p = buf;
    size_t remaining = count;
    while (remaining > 0) {
        ssize_t n = write(fd, p, remaining);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        p += n;
        remaining -= n;
    }
    return 0;
}

int read_exact(int fd, void *buf, size_t count) {
    char *p = buf;
    size_t remaining = count;
    while (remaining > 0) {
        ssize_t n = read(fd, p, remaining);
        if (n < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (n == 0) return -1;
        p += n;
        remaining -= n;
    }
    return 0;
}

int fill_pipe(int write_fd) {
    char buf[4096];
    memset(buf, 'X', sizeof(buf));
    int total = 0;
    while (1) {
        ssize_t n = write(write_fd, buf, sizeof(buf));
        if (n < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) break;
            return -1;
        }
        total += n;
    }
    return total;
}

long long time_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

int create_temp_file(const char *path) {
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) return -1;
    close(fd);
    return 0;
}
