/*
 * bug-poll-wait-user-buffer-race: poll must not keep direct kernel references
 * to the user pollfd array or revents fields while it blocks.
 *
 * Old behavior: poll validated the user pollfd array before sleeping, kept
 * mutable references to each revents field, and later wrote through them
 * directly. If another thread unmapped that array while poll was blocked, the
 * kernel took a page fault when an event woke the waiter.
 *
 * Fixed behavior: poll waits using a kernel pollfd copy and copies the final
 * revents back with checked user-copy. If the userspace array disappears, the
 * syscall returns EFAULT instead of panicking the kernel.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>
#include <stdatomic.h>

static _Atomic int waiter_entered = 0;

struct waiter_context {
    struct pollfd *fds;
    int use_ppoll;
    int result;
    int error;
};

static void *waiter_thread(void *arg)
{
    struct waiter_context *ctx = arg;

    atomic_store_explicit(&waiter_entered, 1, memory_order_release);
    errno = 0;
    if (ctx->use_ppoll) {
        const struct timespec timeout = {
            .tv_sec = 5,
            .tv_nsec = 0,
        };
        ctx->result = ppoll(ctx->fds, 1, &timeout, NULL);
    } else {
        ctx->result = poll(ctx->fds, 1, 5000);
    }
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

static int wake_eventfd(int event_fd)
{
    uint64_t one = 1;
    if (write(event_fd, &one, sizeof(one)) != (ssize_t)sizeof(one)) {
        printf("eventfd write failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }
    return 0;
}

static int run_unmap_test(void)
{
    printf("Starting poll, unmapping its pollfd array, then waking it...\n");

    int event_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (event_fd < 0) {
        printf("eventfd failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    const size_t page_size = 4096;
    struct pollfd *fds = mmap(
        NULL,
        page_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (fds == MAP_FAILED) {
        printf("mmap failed: errno=%d (%s)\n", errno, strerror(errno));
        close(event_fd);
        return 1;
    }

    fds[0].fd = event_fd;
    fds[0].events = POLLIN;
    fds[0].revents = 0;

    struct waiter_context ctx = {
        .fds = fds,
        .use_ppoll = 0,
        .result = 0,
        .error = 0,
    };

    pthread_t waiter;
    int err = pthread_create(&waiter, NULL, waiter_thread, &ctx);
    if (err != 0) {
        printf("pthread_create failed: errno=%d (%s)\n", err, strerror(err));
        munmap(fds, page_size);
        close(event_fd);
        return 1;
    }

    if (wait_for_waiter_to_block() != 0) {
        printf("nanosleep failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (munmap(fds, page_size) != 0) {
        printf("munmap failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (wake_eventfd(event_fd) != 0) {
        return 1;
    }

    err = pthread_join(waiter, NULL);
    if (err != 0) {
        printf("pthread_join failed: errno=%d (%s)\n", err, strerror(err));
        return 1;
    }

    close(event_fd);

    if (ctx.result != -1 || ctx.error != EFAULT) {
        printf(
            "expected poll to fail with EFAULT, got result=%d errno=%d (%s)\n",
            ctx.result,
            ctx.error,
            strerror(ctx.error)
        );
        printf("TEST FAILED\n");
        return 1;
    }

    printf("poll returned EFAULT after the pollfd array was unmapped\n");
    return 0;
}

static int run_revents_only_test(int use_ppoll)
{
    printf(
        "Starting %s, modifying fd/events while blocked, then waking it...\n",
        use_ppoll ? "ppoll" : "poll"
    );

    int event_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    if (event_fd < 0) {
        printf("eventfd failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    struct pollfd fds[1] = {
        {
            .fd = event_fd,
            .events = POLLIN,
            .revents = 0,
        },
    };
    struct waiter_context ctx = {
        .fds = fds,
        .use_ppoll = use_ppoll,
        .result = 0,
        .error = 0,
    };

    atomic_store_explicit(&waiter_entered, 0, memory_order_release);

    pthread_t waiter;
    int err = pthread_create(&waiter, NULL, waiter_thread, &ctx);
    if (err != 0) {
        printf("pthread_create failed: errno=%d (%s)\n", err, strerror(err));
        close(event_fd);
        return 1;
    }

    if (wait_for_waiter_to_block() != 0) {
        printf("nanosleep failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    volatile struct pollfd *volatile_fds = fds;
    volatile_fds[0].fd = -1;
    volatile_fds[0].events = POLLOUT;
    volatile_fds[0].revents = 0;

    if (wake_eventfd(event_fd) != 0) {
        return 1;
    }

    err = pthread_join(waiter, NULL);
    if (err != 0) {
        printf("pthread_join failed: errno=%d (%s)\n", err, strerror(err));
        return 1;
    }

    close(event_fd);

    if (ctx.result != 1 || ctx.error != 0) {
        printf(
            "expected %s to report one event, got result=%d errno=%d (%s)\n",
            use_ppoll ? "ppoll" : "poll",
            ctx.result,
            ctx.error,
            strerror(ctx.error)
        );
        return 1;
    }

    if (fds[0].fd != -1 || fds[0].events != POLLOUT) {
        printf(
            "%s overwrote input fields: fd=%d events=0x%x\n",
            use_ppoll ? "ppoll" : "poll",
            fds[0].fd,
            fds[0].events
        );
        return 1;
    }

    if ((fds[0].revents & POLLIN) == 0) {
        printf(
            "%s did not write POLLIN to revents: revents=0x%x\n",
            use_ppoll ? "ppoll" : "poll",
            fds[0].revents
        );
        return 1;
    }

    printf("%s only updated revents on return\n", use_ppoll ? "ppoll" : "poll");
    return 0;
}

static int run_nfds_limit_test(int use_ppoll)
{
    printf("Checking %s rejects nfds above RLIMIT_NOFILE...\n",
           use_ppoll ? "ppoll" : "poll");

    struct rlimit old_limit;
    if (getrlimit(RLIMIT_NOFILE, &old_limit) != 0) {
        printf("getrlimit failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (old_limit.rlim_max < 1) {
        printf("RLIMIT_NOFILE hard limit is too small for the test\n");
        return 1;
    }

    struct rlimit low_limit = old_limit;
    low_limit.rlim_cur = 1;
    if (setrlimit(RLIMIT_NOFILE, &low_limit) != 0) {
        printf("setrlimit lower failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    struct pollfd fds[2] = {
        {
            .fd = -1,
            .events = 0,
            .revents = 0,
        },
        {
            .fd = -1,
            .events = 0,
            .revents = 0,
        },
    };

    errno = 0;
    int ret;
    if (use_ppoll) {
        const struct timespec timeout = {
            .tv_sec = 0,
            .tv_nsec = 0,
        };
        ret = ppoll(fds, 2, &timeout, NULL);
    } else {
        ret = poll(fds, 2, 0);
    }
    int err = errno;

    if (setrlimit(RLIMIT_NOFILE, &old_limit) != 0) {
        printf("setrlimit restore failed: errno=%d (%s)\n", errno, strerror(errno));
        return 1;
    }

    if (ret != -1 || err != EINVAL) {
        printf(
            "expected %s nfds limit failure EINVAL, got ret=%d errno=%d (%s)\n",
            use_ppoll ? "ppoll" : "poll",
            ret,
            err,
            strerror(err)
        );
        return 1;
    }

    printf("%s rejected nfds above RLIMIT_NOFILE\n", use_ppoll ? "ppoll" : "poll");
    return 0;
}

int main(void)
{
    printf("=== bug-poll-wait-user-buffer-race ===\n");

    atomic_store_explicit(&waiter_entered, 0, memory_order_release);
    if (run_unmap_test() != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    if (run_revents_only_test(0) != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    if (run_revents_only_test(1) != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    if (run_nfds_limit_test(0) != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    if (run_nfds_limit_test(1) != 0) {
        printf("TEST FAILED\n");
        return 1;
    }

    printf("ALL TESTS PASSED\n");
    return 0;
}
