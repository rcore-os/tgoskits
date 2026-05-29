#include "test_framework.h"

#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#define IOCB_CMD_NOOP 6

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
    TEST_START("io_getevents syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "create context for io_getevents");

    if (ctx != 0) {
        struct io_event events[2];
        memset(events, 0, sizeof(events));
        CHECK_RET(syscall(SYS_io_getevents, ctx, 0, 1, events, NULL), 0,
                  "io_getevents returns 0 when no completions are queued");
        CHECK_ERR(syscall(SYS_io_getevents, ctx, 2, 1, events, NULL), EINVAL,
                  "io_getevents rejects min_nr greater than nr");
        CHECK_ERR(syscall(SYS_io_getevents, ctx, -1, 1, events, NULL), EINVAL,
                  "io_getevents rejects negative min_nr");

        struct iocb cb;
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x2222;
        cb.aio_lio_opcode = IOCB_CMD_NOOP;
        struct iocb *list[1] = {&cb};

        CHECK_RET(syscall(SYS_io_submit, ctx, 1, list), 1,
                  "queue one noop completion");
        memset(events, 0, sizeof(events));
        CHECK_RET(syscall(SYS_io_getevents, ctx, 1, 1, events, NULL), 1,
                  "io_getevents returns queued completion");
        CHECK(events[0].data == 0x2222, "io_getevents preserves completion data");
        CHECK(events[0].obj == (uint64_t)(uintptr_t)&cb,
              "io_getevents preserves iocb pointer");
        CHECK(events[0].res == 0 && events[0].res2 == 0,
              "io_getevents reports noop success");
        CHECK_RET(syscall(SYS_io_getevents, ctx, 0, 1, events, NULL), 0,
                  "io_getevents drains completions");

        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy io_getevents context");
    }

    CHECK_ERR(syscall(SYS_io_getevents, 0, 0, 1, NULL, NULL), EINVAL,
              "io_getevents rejects invalid context");
    TEST_DONE();
}
