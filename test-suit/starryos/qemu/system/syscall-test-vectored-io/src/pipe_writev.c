#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

static int failures;

#define REQUIRE(condition) do { \
    if (!(condition)) { perror(#condition); exit(2); } \
} while (0)

static void check(int condition, const char *message)
{
    printf("%s: %s\n", condition ? "PASS" : "FAIL", message);
    failures += !condition;
}

static void check_empty(int fd)
{
    char byte;
    errno = 0;
    check(read(fd, &byte, 1) == -1 && errno == EAGAIN,
          "no bytes are committed beyond the reported prefix");
}

/* One write step commits complete pages before encountering a bad segment.
 * Small writes must not publish the valid half of a faulting atomic chunk.
 */
static void fault_within_write_step(size_t page)
{
    int pipefd[2];
    REQUIRE(pipe2(pipefd, O_NONBLOCK) == 0);
    char *source = mmap(NULL, 3 * page, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(source != MAP_FAILED);
    memset(source, 'S', 2 * page);
    REQUIRE(mprotect(source + 2 * page, page, PROT_NONE) == 0);
    struct iovec iov[] = {
        { .iov_base = source, .iov_len = 2 * page },
        { .iov_base = NULL, .iov_len = 0 },
        { .iov_base = source + 2 * page, .iov_len = page },
    };
    long result = syscall(SYS_writev, pipefd[1], iov, 3);
    check(result == (long)(2 * page), "writev returns committed pages before EFAULT");
    char *received = malloc(2 * page);
    REQUIRE(received != NULL);
    ssize_t count = read(pipefd[0], received, 2 * page);
    check(count == (ssize_t)(2 * page) && memcmp(received, source, 2 * page) == 0,
          "faulting writev publishes exactly its successful pages");
    check_empty(pipefd[0]);

    iov[0].iov_len = page / 2;
    iov[2].iov_len = page / 2;
    errno = 0;
    result = syscall(SYS_writev, pipefd[1], iov, 3);
    check(result == -1 && errno == EFAULT, "faulting PIPE_BUF write reports EFAULT");
    check_empty(pipefd[0]);

    /* Scalar writes share the pipe commit path and must preserve progress too. */
    result = syscall(SYS_write, pipefd[1], source, 3 * page);
    check(result == (long)(2 * page), "write returns committed pages before EFAULT");
    count = read(pipefd[0], received, 2 * page);
    check(count == result && count == (ssize_t)(2 * page)
              && memcmp(received, source, 2 * page) == 0,
          "scalar write return value matches committed bytes");
    check_empty(pipefd[0]);

    REQUIRE(write(pipefd[1], "M", 1) == 1);
    iov[0].iov_base = source + page;
    iov[0].iov_len = page;
    iov[2].iov_len = 1;
    result = syscall(SYS_writev, pipefd[1], iov, 3);
    check(result == 1, "successful pipe merge survives a fault in the next chunk");
    count = read(pipefd[0], received, 2 * page);
    check(count == 2 && memcmp(received, "MS", 2) == 0,
          "failed chunk bytes are not counted or published after a merge");
    check_empty(pipefd[0]);
    free(received);
    REQUIRE(munmap(source, 3 * page) == 0);
    close(pipefd[0]);
    close(pipefd[1]);
}

struct blocked_write {
    int fd;
    struct iovec segments[2];
    long result;
    int error;
};

static void *write_until_fault(void *argument)
{
    struct blocked_write *request = argument;
    request->result = syscall(SYS_writev, request->fd, request->segments, 2);
    request->error = errno;
    return NULL;
}

/* A full pipe is the synchronization point: the writer has committed the
 * prefix and cannot read its tail until the reader releases pipe capacity.
 */
static void unmap_blocked_write_tail(size_t page)
{
    int pipefd[2];
    REQUIRE(pipe(pipefd) == 0);
    int capacity = fcntl(pipefd[1], F_GETPIPE_SZ);
    REQUIRE(capacity > 0);
    char *prefix = malloc((size_t)capacity);
    char *received = malloc((size_t)capacity);
    REQUIRE(prefix != NULL && received != NULL);
    memset(prefix, 'B', (size_t)capacity);
    char *tail = mmap(NULL, page, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(tail != MAP_FAILED);
    memset(tail, 'T', page);
    struct blocked_write request = {
        .fd = pipefd[1],
        .segments = {
            { .iov_base = prefix, .iov_len = (size_t)capacity },
            { .iov_base = tail, .iov_len = page },
        },
    };
    pthread_t writer;
    REQUIRE(pthread_create(&writer, NULL, write_until_fault, &request) == 0);
    struct pollfd readable = { .fd = pipefd[0], .events = POLLIN };
    REQUIRE(poll(&readable, 1, 10000) == 1);
    int available = 0;
    REQUIRE(ioctl(pipefd[0], FIONREAD, &available) == 0);
    REQUIRE(available == capacity);
    REQUIRE(munmap(tail, page) == 0);
    REQUIRE(read(pipefd[0], received, (size_t)capacity) == capacity);
    REQUIRE(pthread_join(writer, NULL) == 0);
    check(request.result == capacity,
          "concurrent munmap preserves the blocked writev committed count");
    if (request.result != capacity)
        printf("writev result=%ld errno=%d expected=%d\n",
               request.result, request.error, capacity);
    check(memcmp(received, prefix, (size_t)capacity) == 0,
          "blocked writev prefix content matches its reported progress");
    REQUIRE(fcntl(pipefd[0], F_SETFL, O_NONBLOCK) == 0);
    check_empty(pipefd[0]);
    free(received);
    free(prefix);
    close(pipefd[0]);
    close(pipefd[1]);
}

static void lazy_disjoint_segments(size_t page)
{
    const size_t segment_size = 1024 * 1024;
    const size_t segment_count = 1024;
    int pipefd[2];
    REQUIRE(pipe2(pipefd, O_NONBLOCK) == 0);
    int capacity = fcntl(pipefd[1], F_GETPIPE_SZ);
    REQUIRE(capacity > 0);
    struct iovec segments[1024];
    for (size_t i = 0; i < segment_count; i++) {
        void *mapping = mmap(NULL, segment_size, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        REQUIRE(mapping != MAP_FAILED);
        segments[i] = (struct iovec){ .iov_base = mapping, .iov_len = segment_size };
    }
    /* Observe the last page without touching it, before and after writev. */
    char *last = (char *)segments[segment_count - 1].iov_base + segment_size - page;
    unsigned char resident = 0xff;
    REQUIRE(syscall(SYS_mincore, last, page, &resident) == 0);
    REQUIRE((resident & 1) == 0);
    long result = syscall(SYS_writev, pipefd[1], segments, segment_count);
    check(result == capacity, "disjoint lazy 1 GiB writev needs only pipe capacity");
    REQUIRE(syscall(SYS_mincore, last, page, &resident) == 0);
    check((resident & 1) == 0, "unused lazy tail remains nonresident");
    REQUIRE(mprotect(last, page, PROT_NONE) == 0);
    struct iovec unavailable = { .iov_base = last, .iov_len = page };
    errno = 0;
    result = syscall(SYS_writev, pipefd[1], &unavailable, 1);
    check(result == -1 && errno == EAGAIN,
          "full nonblocking pipe rejects writev without accessing its payload");
    char *received = malloc((size_t)capacity);
    REQUIRE(received != NULL);
    ssize_t count = read(pipefd[0], received, (size_t)capacity);
    int zeroed = count == capacity;
    for (ssize_t i = 0; i < count; i++)
        zeroed &= received[i] == 0;
    check(zeroed, "lazy anonymous prefix reads back as zeroes");
    free(received);
    for (size_t i = 0; i < segment_count; i++)
        REQUIRE(munmap(segments[i].iov_base, segment_size) == 0);
    close(pipefd[0]);
    close(pipefd[1]);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    alarm(90);
    long page = sysconf(_SC_PAGESIZE);
    REQUIRE(page > 0);
    fault_within_write_step((size_t)page);
    unmap_blocked_write_tail((size_t)page);
    lazy_disjoint_segments((size_t)page);
    printf("pipe writev: %d failures\n", failures);
    return failures != 0;
}
