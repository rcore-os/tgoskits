#include "test_framework.h"

#include <signal.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define IOCB_CMD_NOOP 6
#define KERNEL_SIGSET_SIZE 8

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

struct aio_sigset_arg {
    const sigset_t *sigmask;
    size_t sigsetsize;
};

static void test_aio_read_completion_after_fd_close(void)
{
    static char buffer[8];
    const char expected[8] = "aio-data";
    int fd = (int)syscall(SYS_memfd_create, "aio-read-owner", 0);
    CHECK(fd >= 0, "create AIO read source");
    if (fd < 0)
        return;
    CHECK_RET(write(fd, expected, sizeof(expected)), sizeof(expected),
              "initialize AIO read source");
    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0, "create read completion context");
    if (ctx == 0) {
        close(fd);
        return;
    }
    memset(buffer, 0, sizeof(buffer));
    struct iocb cb = {0};
    cb.aio_data = 0x3344;
    cb.aio_lio_opcode = 0; /* IOCB_CMD_PREAD */
    cb.aio_fildes = (uint32_t)fd;
    cb.aio_buf = (uint64_t)(uintptr_t)buffer;
    cb.aio_nbytes = sizeof(buffer);
    struct iocb *requests[] = {&cb};
    long submitted = syscall(SYS_io_submit, ctx, 1, requests);
    CHECK(submitted == 1, "submit real AIO read");
    CHECK_RET(close(fd), 0, "close fd after request owns the source");
    if (submitted == 1) {
        struct io_event event = {0};
        struct timespec timeout = {1, 0};
        CHECK_RET(syscall(SYS_io_getevents, ctx, 1, 1, &event, &timeout), 1,
                  "io_getevents returns real read completion");
        CHECK(event.data == cb.aio_data && event.obj == (uint64_t)(uintptr_t)&cb,
              "completion retains request identity");
        CHECK(event.res == sizeof(buffer) && event.res2 == 0,
              "completion reports exact byte count");
        CHECK(memcmp(buffer, expected, sizeof(buffer)) == 0,
              "request wrote the caller's address space after fd close");
    }
    CHECK_RET(syscall(SYS_io_destroy, ctx), 0, "retire completed read context");
}

int main(void)
{
    TEST_START("io_pgetevents syscall semantics");
    test_aio_read_completion_after_fd_close();

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "create context for io_pgetevents");

    if (ctx != 0) {
        struct io_event event;
        memset(&event, 0, sizeof(event));
        struct timespec zero_timeout = {0, 0};
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 0, 1, &event, NULL, 0), 0,
                  "io_pgetevents returns 0 when no completions are queued");
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 1, 1, &event, &zero_timeout, 0), 0,
                  "io_pgetevents honors zero timeout when min_nr waits");
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 0, 0, NULL, NULL, 0), 0,
                  "io_pgetevents with nr 0 returns 0");
        CHECK_ERR(syscall(SYS_io_pgetevents, ctx, 2, 1, &event, NULL, 0), EINVAL,
                  "io_pgetevents rejects min_nr greater than nr");
        CHECK_ERR(syscall(SYS_io_pgetevents, ctx, (long)-1, 1, &event, NULL, 0), EINVAL,
                  "io_pgetevents rejects negative min_nr");

        struct iocb cb;
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x3333;
        cb.aio_lio_opcode = IOCB_CMD_NOOP;
        struct iocb *list[1] = {&cb};

        CHECK_RET(syscall(SYS_io_submit, ctx, 1, list), 1,
                  "queue one noop completion for io_pgetevents");
        memset(&event, 0, sizeof(event));
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 1, 1, &event, NULL, 0), 1,
                  "io_pgetevents returns queued completion");
        CHECK(event.data == 0x3333, "io_pgetevents preserves completion data");
        CHECK(event.obj == (uint64_t)(uintptr_t)&cb,
              "io_pgetevents preserves iocb pointer");
        CHECK(event.res == 0 && event.res2 == 0,
              "io_pgetevents reports noop success");

        sigset_t mask;
        sigemptyset(&mask);
        sigaddset(&mask, SIGUSR1);
        struct aio_sigset_arg sigarg = {
            .sigmask = &mask,
            .sigsetsize = KERNEL_SIGSET_SIZE,
        };
        memset(&event, 0, sizeof(event));
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 0, 1, &event, &zero_timeout, &sigarg), 0,
                  "io_pgetevents accepts a valid raw sigmask wrapper");

        sigarg.sigmask = NULL;
        sigarg.sigsetsize = KERNEL_SIGSET_SIZE;
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 0, 1, &event, &zero_timeout, &sigarg), 0,
                  "io_pgetevents accepts wrapper with NULL inner sigmask");

        sigarg.sigmask = &mask;
        sigarg.sigsetsize = KERNEL_SIGSET_SIZE - 1;
        CHECK_ERR(syscall(SYS_io_pgetevents, ctx, 0, 1, &event, &zero_timeout, &sigarg), EINVAL,
                  "io_pgetevents rejects invalid sigsetsize");
        CHECK_ERR(syscall(SYS_io_pgetevents, ctx, 0, 1, &event, &zero_timeout,
                          (void *)(uintptr_t)1),
                  EFAULT,
                  "io_pgetevents rejects invalid sigmask wrapper pointer");

        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy io_pgetevents context");
    }

    CHECK_ERR(syscall(SYS_io_pgetevents, 0, 0, 1, NULL, NULL, 0), EINVAL,
              "io_pgetevents rejects invalid context");
    TEST_DONE();
}
