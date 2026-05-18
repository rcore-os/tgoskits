/*
 * bug-epoll-wait-user-buffer-race: epoll_wait must not keep a direct
 * kernel reference to the user events buffer while it blocks.
 *
 * Old behavior: epoll_wait validated the user buffer before sleeping, kept a
 * raw &mut [epoll_event], and later wrote into it directly. If another thread
 * unmapped that buffer while epoll_wait was blocked, the kernel took a page
 * fault when an event woke the waiter.
 *
 * Fixed behavior: epoll_wait stores ready events in a kernel buffer while
 * waiting and copies them back with checked user-copy. If the userspace buffer
 * disappears, the syscall returns EFAULT instead of panicking the kernel.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <stdatomic.h>

static _Atomic int waiter_entered = 0;

struct waiter_context {
    int epfd;
    struct epoll_event *events;
    int result;
    int error;
};

static void *waiter_thread(void *arg)
{
    struct waiter_context *ctx = arg;

    atomic_store_explicit(&waiter_entered, 1, memory_order_release);
    errno = 0;
    ctx->result = epoll_wait(ctx->epfd, ctx->events, 1, 5000);
    ctx->error = errno;
    return NULL;
}

static int wait_for_waiter_to_block(void)
{
    const struct timespec settle = {
        .tv_sec = 0,
        .tv_nsec = 100 * 1000 * 1000,
    };

    while (atomic_load_explicit(&waiter_entered, memory_order_acquire) == 0) {
        sched_yield();
    }

    return nanosleep(&settle, NULL);
}

static int run_unmap_test(void)
{
    printf("Starting epoll_wait, unmapping its output buffer, then waking it...\n");

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        printf("epoll_create1 failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    int event_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (event_fd < 0) {
        printf("eventfd failed: errno=%d (%s)\n", errno, strerror(errno));
        close(epfd);
        return 1;
    }

    struct epoll_event interest = {
        .events = EPOLLIN,
        .data.u64 = 0x1234,
    };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, event_fd, &interest) != 0) {
        printf("epoll_ctl ADD failed: errno=%d (%s)\n", errno, strerror(errno));
        close(event_fd);
        close(epfd);
        return 1;
    }

    const size_t page_size = 4096;
    struct epoll_event *events = mmap(
        NULL,
        page_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (events == MAP_FAILED) {
        printf("mmap failed: errno=%d (%s)\n", errno, strerror(errno));
        close(event_fd);
        close(epfd);
        return 1;
    }

    struct waiter_context ctx = {
        .epfd = epfd,
        .events = events,
        .result = 0,
        .error = 0,
    };

    pthread_t waiter;
    int err = pthread_create(&waiter, NULL, waiter_thread, &ctx);
    if (err != 0) {
        printf("pthread_create failed: errno=%d (%s)\n", err, strerror(err));
        munmap(events, page_size);
        close(event_fd);
        close(epfd);
        return 1;
    }

    if (wait_for_waiter_to_block() != 0) {
        printf("nanosleep failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (munmap(events, page_size) != 0) {
        printf("munmap failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    uint64_t one = 1;
    if (write(event_fd, &one, sizeof(one)) != (ssize_t)sizeof(one)) {
        printf("eventfd write failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    err = pthread_join(waiter, NULL);
    if (err != 0) {
        printf("pthread_join failed: errno=%d (%s)\n", err, strerror(err));
        return 1;
    }

    close(event_fd);
    close(epfd);

    if (ctx.result != -1 || ctx.error != EFAULT) {
        printf(
            "expected epoll_wait to fail with EFAULT, got result=%d errno=%d (%s)\n",
            ctx.result,
            ctx.error,
            strerror(ctx.error)
        );
        printf("TEST FAILED\n");
        return 1;
    }

    printf("epoll_wait returned EFAULT after the user buffer was unmapped\n");
    return 0;
}

static int run_partial_copy_requeue_test(void)
{
    printf("Starting epoll_wait with a partially invalid output array...\n");

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        printf("epoll_create1 failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    int event_fd1 = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    int event_fd2 = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (event_fd1 < 0 || event_fd2 < 0) {
        printf("eventfd failed: errno=%d (%s)\n", errno, strerror(errno));
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    const uint64_t data1 = 0x11111111;
    const uint64_t data2 = 0x22222222;
    struct epoll_event interest1 = {
        .events = EPOLLIN | EPOLLONESHOT,
        .data.u64 = data1,
    };
    struct epoll_event interest2 = {
        .events = EPOLLIN | EPOLLONESHOT,
        .data.u64 = data2,
    };

    if (epoll_ctl(epfd, EPOLL_CTL_ADD, event_fd1, &interest1) != 0 ||
        epoll_ctl(epfd, EPOLL_CTL_ADD, event_fd2, &interest2) != 0) {
        printf("epoll_ctl ADD failed: errno=%d (%s)\n", errno, strerror(errno));
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    const size_t page_size = 4096;
    void *mapping = mmap(
        NULL,
        page_size * 2,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (mapping == MAP_FAILED) {
        printf("mmap failed: errno=%d (%s)\n", errno, strerror(errno));
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    if (mprotect((char *)mapping + page_size, page_size, PROT_NONE) != 0) {
        printf("mprotect failed: errno=%d (%s)\n", errno, strerror(errno));
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    struct epoll_event *events =
        (struct epoll_event *)((char *)mapping + page_size - sizeof(struct epoll_event));

    uint64_t one = 1;
    if (write(event_fd1, &one, sizeof(one)) != (ssize_t)sizeof(one) ||
        write(event_fd2, &one, sizeof(one)) != (ssize_t)sizeof(one)) {
        printf("eventfd write failed: errno=%d (%s)\n", errno, strerror(errno));
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    errno = 0;
    int ret = epoll_wait(epfd, events, 2, 0);
    int err = errno;
    if (ret != 1 || err != 0) {
        printf(
            "expected first epoll_wait to copy one event, got ret=%d errno=%d (%s)\n",
            ret,
            err,
            strerror(err)
        );
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    uint64_t first_data = events[0].data.u64;
    if (first_data != data1 && first_data != data2) {
        printf("first epoll_wait returned unexpected data=0x%llx\n",
               (unsigned long long)first_data);
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    if (mprotect((char *)mapping + page_size, page_size, PROT_READ | PROT_WRITE) != 0) {
        printf("mprotect restore failed: errno=%d (%s)\n", errno, strerror(errno));
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    errno = 0;
    ret = epoll_wait(epfd, events, 2, 0);
    err = errno;
    if (ret != 1 || err != 0) {
        printf(
            "expected failed event to be requeued, got ret=%d errno=%d (%s)\n",
            ret,
            err,
            strerror(err)
        );
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    if (events[0].data.u64 == first_data ||
        (events[0].data.u64 != data1 && events[0].data.u64 != data2)) {
        printf(
            "expected the second epoll_wait to return the event whose copy failed, got first=0x%llx second=0x%llx\n",
            (unsigned long long)first_data,
            (unsigned long long)events[0].data.u64
        );
        munmap(mapping, page_size * 2);
        close(event_fd1);
        close(event_fd2);
        close(epfd);
        return 1;
    }

    munmap(mapping, page_size * 2);
    close(event_fd1);
    close(event_fd2);
    close(epfd);

    printf("epoll_wait requeued the event whose user copy failed\n");
    return 0;
}

static int run_maxevents_limit_test(void)
{
    printf("Checking epoll_wait maxevents limit...\n");

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        printf("epoll_create1 failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    struct epoll_event event;
    errno = 0;
    int ret = epoll_wait(epfd, &event, INT_MAX, 0);
    int err = errno;
    close(epfd);

    if (ret != -1 || err != EINVAL) {
        printf(
            "expected epoll_wait with excessive maxevents to fail with EINVAL, got ret=%d errno=%d (%s)\n",
            ret,
            err,
            strerror(err)
        );
        return 1;
    }

    printf("epoll_wait rejected excessive maxevents\n");
    return 0;
}

static int run_invalid_user_range_no_ready_test(void)
{
    printf("Checking epoll_wait rejects invalid user range before waiting...\n");

    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        printf("epoll_create1 failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    struct epoll_event *bad_events = (struct epoll_event *)(UINTPTR_MAX - 4095);
    errno = 0;
    int ret = epoll_wait(epfd, bad_events, 1, 0);
    int err = errno;
    if (ret != -1 || err != EFAULT) {
        printf(
            "expected epoll_wait with out-of-user events to fail with EFAULT, got ret=%d errno=%d (%s)\n",
            ret,
            err,
            strerror(err)
        );
        close(epfd);
        return 1;
    }

    bad_events =
        (struct epoll_event *)(UINTPTR_MAX - sizeof(struct epoll_event) / 2);
    errno = 0;
    ret = epoll_wait(epfd, bad_events, 1, 0);
    err = errno;
    close(epfd);
    if (ret != -1 || err != EFAULT) {
        printf(
            "expected epoll_wait with overflowing events range to fail with EFAULT, got ret=%d errno=%d (%s)\n",
            ret,
            err,
            strerror(err)
        );
        return 1;
    }

    printf("epoll_wait rejected invalid user ranges without ready events\n");
    return 0;
}

int main(void)
{
    printf("=== bug-epoll-wait-user-buffer-race ===\n");

    if (run_unmap_test() != 0 ||
        run_partial_copy_requeue_test() != 0 ||
        run_maxevents_limit_test() != 0 ||
        run_invalid_user_range_no_ready_test() != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    printf("ALL TESTS PASSED\n");
    return 0;
}
