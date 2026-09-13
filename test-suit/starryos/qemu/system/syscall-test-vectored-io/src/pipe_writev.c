#define _GNU_SOURCE
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

#define REQUIRE(condition) do { \
    if (!(condition)) { perror(#condition); exit(2); } \
} while (0)

/* Cover progress lost inside a single fallible pipe write step. */
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
        { .iov_base = source + 2 * page, .iov_len = page },
    };
    REQUIRE(syscall(SYS_writev, pipefd[1], iov, 2) == (long)(2 * page));
    char *received = malloc(2 * page);
    REQUIRE(received != NULL);
    REQUIRE(read(pipefd[0], received, 2 * page) == (ssize_t)(2 * page));
    REQUIRE(memcmp(received, source, 2 * page) == 0);
    int available;
    REQUIRE(ioctl(pipefd[0], FIONREAD, &available) == 0 && available == 0);
    free(received);
    REQUIRE(munmap(source, 3 * page) == 0);
    close(pipefd[0]);
    close(pipefd[1]);
    puts("PASS: writev retains progress within one write step");
}

struct blocked_write {
    int fd;
    struct iovec segments[2];
    long result;
};

static void *write_until_fault(void *argument)
{
    struct blocked_write *request = argument;
    request->result = syscall(SYS_writev, request->fd, request->segments, 2);
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
    REQUIRE(request.result == capacity);
    REQUIRE(memcmp(received, prefix, (size_t)capacity) == 0);
    REQUIRE(ioctl(pipefd[0], FIONREAD, &available) == 0 && available == 0);
    free(received);
    free(prefix);
    close(pipefd[0]);
    close(pipefd[1]);
    puts("PASS: writev retains progress after concurrent munmap");
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
    REQUIRE(syscall(SYS_writev, pipefd[1], segments, segment_count) == capacity);
    REQUIRE(syscall(SYS_mincore, last, page, &resident) == 0 && (resident & 1) == 0);
    char *received = malloc((size_t)capacity);
    REQUIRE(received != NULL);
    REQUIRE(read(pipefd[0], received, (size_t)capacity) == capacity);
    for (int i = 0; i < capacity; i++)
        REQUIRE(received[i] == 0);
    free(received);
    for (size_t i = 0; i < segment_count; i++)
        REQUIRE(munmap(segments[i].iov_base, segment_size) == 0);
    close(pipefd[0]);
    close(pipefd[1]);
    puts("PASS: disjoint lazy writev leaves the unused tail nonresident");
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
    return 0;
}
