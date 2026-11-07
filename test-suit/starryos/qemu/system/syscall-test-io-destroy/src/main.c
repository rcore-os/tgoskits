#include "test_framework.h"

#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define IOCB_CMD_POLL 5

typedef unsigned long aio_context_t;

struct io_event {
    uint64_t data;
    uint64_t obj;
    int64_t res;
    int64_t res2;
};

struct iocb {
    uint64_t aio_data;
    uint32_t aio_key;
    uint32_t aio_rw_flags;
    uint16_t aio_lio_opcode;
    int16_t aio_reqprio;
    uint32_t aio_fildes;
    uint64_t aio_buf;
    uint64_t aio_nbytes;
    int64_t aio_offset;
    uint64_t aio_reserved2;
    uint32_t aio_flags;
    uint32_t aio_resfd;
};

struct waiter_args {
    aio_context_t ctx;
    volatile struct io_event *events;
    long result;
    int error;
};

static void *wait_for_two_events(void *opaque)
{
    struct waiter_args *args = opaque;

    errno = 0;
    args->result = syscall(SYS_io_getevents, args->ctx, 2, 2, args->events, NULL);
    args->error = errno;
    return NULL;
}

static void child_exit_without_io_destroy(void)
{
    pid_t child = fork();
    CHECK(child >= 0, "fork child for implicit AIO cleanup");
    if (child == 0) {
        aio_context_t child_ctx = 0;
        if (syscall(SYS_io_setup, 4, &child_ctx) != 0 || child_ctx == 0) {
            _exit(10);
        }

        int child_pipe[2] = {-1, -1};
        if (pipe(child_pipe) != 0) {
            _exit(11);
        }

        struct iocb cb;
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x6262;
        cb.aio_lio_opcode = IOCB_CMD_POLL;
        cb.aio_fildes = (uint32_t)child_pipe[0];
        cb.aio_buf = POLLIN;
        struct iocb *list[1] = {&cb};
        if (syscall(SYS_io_submit, child_ctx, 1, list) != 1) {
            _exit(12);
        }

        _exit(0);
    }

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child,
              "wait child that exits without io_destroy");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child exits cleanly after implicit AIO cleanup");
}

static void destroy_with_blocked_getevents(void)
{
    /* The waiter owns an AioContext while io_destroy removes it from the
     * global table. The ready pipe supplies a real POLL completion; after the
     * first event is visible, FIFO keeps the lower-priority waiter off this
     * CPU while destroy closes admission, waits for that waiter, and unmaps.
     */
    aio_context_t ctx = 0;
    aio_context_t ring_addr = 0;
    int pipefd[2] = {-1, -1};
    pthread_t waiter;
    int waiter_created = 0;
    int affinity_changed = 0;
    int policy_changed = 0;
    int saved_policy = SCHED_OTHER;
    struct sched_param saved_param = {0};
    struct sched_param fifo_param = {.sched_priority = 99};
    cpu_set_t saved_affinity;
    cpu_set_t single_cpu;
    volatile struct io_event events[2];
    struct waiter_args args;
    struct iocb cb;
    struct iocb *list[1] = {&cb};
    long submitted;
    long destroyed;

    CPU_ZERO(&saved_affinity);
    CPU_ZERO(&single_cpu);
    memset((void *)events, 0, sizeof(events));
    memset(&args, 0, sizeof(args));
    memset(&cb, 0, sizeof(cb));

    CHECK_RET(syscall(SYS_io_setup, 8, &ctx), 0,
              "create context for concurrent getevents destroy");
    if (ctx == 0) {
        goto cleanup;
    }
    ring_addr = ctx;

    if (pipe(pipefd) != 0) {
        CHECK(0, "create ready pipe for blocked getevents");
        goto cleanup;
    }
    if (write(pipefd[1], "R", 1) != 1) {
        CHECK(0, "make poll request ready");
        goto cleanup;
    }
    saved_policy = (int)syscall(SYS_sched_getscheduler, 0);
    if (saved_policy < 0 || syscall(SYS_sched_getparam, 0, &saved_param) != 0) {
        CHECK(0, "read original scheduling policy and parameter");
        goto cleanup;
    }
    if (syscall(SYS_sched_getaffinity, 0, sizeof(saved_affinity), &saved_affinity) < 0) {
        CHECK(0, "read original affinity");
        goto cleanup;
    }
    int test_cpu = -1;
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        if (CPU_ISSET(cpu, &saved_affinity)) {
            test_cpu = cpu;
            break;
        }
    }
    if (test_cpu < 0) {
        CHECK(0, "find an allowed CPU");
        goto cleanup;
    }
    CPU_SET(test_cpu, &single_cpu);
    if (syscall(SYS_sched_setaffinity, 0, sizeof(single_cpu), &single_cpu) != 0) {
        CHECK(0, "pin concurrent getevents test to one CPU");
        goto cleanup;
    }
    affinity_changed = 1;

    args.ctx = ctx;
    args.events = events;
    if (pthread_create(&waiter, NULL, wait_for_two_events, &args) != 0) {
        CHECK(0, "start blocked io_getevents waiter");
        goto cleanup;
    }
    waiter_created = 1;

    cb.aio_data = 0x6262;
    cb.aio_lio_opcode = IOCB_CMD_POLL;
    cb.aio_fildes = (uint32_t)pipefd[0];
    cb.aio_buf = POLLIN;
    submitted = syscall(SYS_io_submit, ctx, 1, list);
    CHECK_RET(submitted, 1, "queue one completion for blocked waiter");
    if (submitted != 1) {
        goto cleanup;
    }

    int observed = 0;
    for (int i = 0; i < 100000; i++) {
        if (__atomic_load_n((const uint64_t *)&events[0].data,
                            __ATOMIC_ACQUIRE) == cb.aio_data) {
            observed = 1;
            break;
        }
        /* This yield only waits for the real completion; the destroy ordering
         * is established by the FIFO transition below. */
        sched_yield();
    }
    CHECK(observed, "waiter copied the first completion before destroy");
    if (!observed) {
        goto cleanup;
    }

    if (syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &fifo_param) != 0) {
        CHECK(0, "temporarily raise main thread to SCHED_FIFO");
        goto cleanup;
    }
    policy_changed = 1;
    destroyed = syscall(SYS_io_destroy, ctx);
    CHECK_RET(destroyed, 0, "destroy context while waiter needs another completion");
    if (destroyed != 0) {
        goto cleanup;
    }
    errno = 0;
    unsigned char resident = 0;
    long mapped = syscall(SYS_mincore, (void *)(uintptr_t)ring_addr, 1, &resident);
    CHECK(mapped == -1 && errno == ENOMEM,
          "destroy releases the ring mapping before returning");
    ctx = 0;
    if (pthread_join(waiter, NULL) != 0) {
        CHECK(0, "join getevents waiter after destroy");
        goto cleanup;
    }
    waiter_created = 0;
    CHECK_RET(args.result, 1, "io_getevents returns copied completions after destroy");
    CHECK(args.error == 0, "destroy does not turn blocked getevents into EFAULT");
    CHECK(args.events[0].data == cb.aio_data, "first completion preserves data");
    CHECK(args.events[0].obj == (uint64_t)(uintptr_t)&cb, "first completion preserves obj");
    CHECK((args.events[0].res & POLLIN) != 0, "first completion reports POLLIN");
    CHECK(args.events[0].res2 == 0, "first completion reports zero res2");

cleanup:
    if (waiter_created) {
        if (ctx != 0) {
            syscall(SYS_io_destroy, ctx);
            ctx = 0;
        }
        pthread_join(waiter, NULL);
    }
    if (ctx != 0) {
        syscall(SYS_io_destroy, ctx);
    }
    if (policy_changed) {
        CHECK_RET(syscall(SYS_sched_setscheduler, 0, saved_policy, &saved_param), 0,
                  "restore original scheduling policy and parameter");
    }
    if (affinity_changed) {
        CHECK_RET(syscall(SYS_sched_setaffinity, 0, sizeof(saved_affinity), &saved_affinity), 0,
                  "restore original affinity");
    }
    if (pipefd[0] >= 0) {
        CHECK_RET(close(pipefd[0]), 0, "close ready pipe read end");
    }
    if (pipefd[1] >= 0) {
        CHECK_RET(close(pipefd[1]), 0, "close ready pipe write end");
    }
}

int main(void)
{
    TEST_START("io_destroy syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 2, &ctx), 0,
              "create context for io_destroy");
    CHECK(ctx != 0, "io_setup returned a usable context");
    if (ctx != 0) {
        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "io_destroy removes a valid context");
        CHECK_ERR(syscall(SYS_io_destroy, ctx), EINVAL,
                  "io_destroy rejects a destroyed context");
    }

    ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "create context for io_destroy with in-flight poll");
    int pipefd[2] = {-1, -1};
    CHECK_RET(pipe(pipefd), 0, "create pipe for blocking aio poll");
    if (ctx != 0 && pipefd[0] >= 0) {
        struct iocb cb;
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x5151;
        cb.aio_lio_opcode = IOCB_CMD_POLL;
        cb.aio_fildes = (uint32_t)pipefd[0];
        cb.aio_buf = POLLIN;
        struct iocb *list[1] = {&cb};

        CHECK_RET(syscall(SYS_io_submit, ctx, 1, list), 1,
                  "queue blocking poll request before io_destroy");
        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "io_destroy drains in-flight blocking request");
        CHECK_ERR(syscall(SYS_io_destroy, ctx), EINVAL,
                  "destroyed in-flight context is removed");
    }
    if (pipefd[0] >= 0) {
        CHECK_RET(close(pipefd[0]), 0, "close poll pipe read end");
    }
    if (pipefd[1] >= 0) {
        CHECK_RET(close(pipefd[1]), 0, "close poll pipe write end");
    }

    destroy_with_blocked_getevents();
    child_exit_without_io_destroy();

    CHECK_ERR(syscall(SYS_io_destroy, 0), EINVAL,
              "io_destroy rejects context 0");
    CHECK_ERR(syscall(SYS_io_destroy, 0x7fffffffUL), EINVAL,
              "io_destroy rejects an unknown context id");

    TEST_DONE();
}
