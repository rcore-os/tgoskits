#include "test_framework.h"

#include <poll.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#define IOCB_CMD_POLL 5
#ifndef ECANCELED
#define ECANCELED 125
#endif

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

int main(void)
{
    TEST_START("io_cancel syscall compatibility");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 8, &ctx), 0,
              "create context for io_cancel");

    if (ctx != 0) {
        struct iocb cb;
        struct io_event event;
        memset(&cb, 0, sizeof(cb));
        memset(&event, 0, sizeof(event));

        CHECK_ERR(syscall(SYS_io_cancel, ctx, &cb, &event), EINVAL,
                  "io_cancel reports unavailable for completed operations");

        int pipefd[2] = {-1, -1};
        CHECK_RET(pipe(pipefd), 0, "create pipe for queued cancel test");
        if (pipefd[0] >= 0) {
            struct iocb poll_cb[6];
            struct iocb *list[6];
            memset(poll_cb, 0, sizeof(poll_cb));
            for (int i = 0; i < 6; i++) {
                poll_cb[i].aio_data = 0x7000u + (uint64_t)i;
                poll_cb[i].aio_lio_opcode = IOCB_CMD_POLL;
                poll_cb[i].aio_fildes = (uint32_t)pipefd[0];
                poll_cb[i].aio_buf = POLLIN;
                list[i] = &poll_cb[i];
            }

            CHECK_RET(syscall(SYS_io_submit, ctx, 6, list), 6,
                      "queue more poll requests than AIO workers");
            memset(&event, 0, sizeof(event));
            CHECK_RET(syscall(SYS_io_cancel, ctx, &poll_cb[5], &event), 0,
                      "io_cancel cancels a request still waiting in the queue");
            CHECK(event.data == 0x7005u,
                  "io_cancel returns canceled request data");
            CHECK(event.obj == (uint64_t)(uintptr_t)&poll_cb[5],
                  "io_cancel returns canceled iocb pointer");
            CHECK(event.res == -ECANCELED && event.res2 == 0,
                  "io_cancel reports ECANCELED in the result event");
            CHECK_ERR(syscall(SYS_io_cancel, ctx, &poll_cb[5], &event), EINVAL,
                      "io_cancel rejects an already canceled request");

            CHECK_RET(close(pipefd[0]), 0, "close cancel pipe read end");
            CHECK_RET(close(pipefd[1]), 0, "close cancel pipe write end");
        }

        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy io_cancel context");
    }

    CHECK_ERR(syscall(SYS_io_cancel, 0, NULL, NULL), EINVAL,
              "io_cancel rejects invalid context");
    TEST_DONE();
}
