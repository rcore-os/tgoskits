#include "test_framework.h"

#include <poll.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define IOCB_CMD_POLL 5

typedef unsigned long aio_context_t;

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

    child_exit_without_io_destroy();

    CHECK_ERR(syscall(SYS_io_destroy, 0), EINVAL,
              "io_destroy rejects context 0");
    CHECK_ERR(syscall(SYS_io_destroy, 0x7fffffffUL), EINVAL,
              "io_destroy rejects an unknown context id");

    TEST_DONE();
}
