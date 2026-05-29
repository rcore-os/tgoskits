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
    TEST_START("io_pgetevents syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "create context for io_pgetevents");

    if (ctx != 0) {
        struct io_event event;
        memset(&event, 0, sizeof(event));
        CHECK_RET(syscall(SYS_io_pgetevents, ctx, 0, 1, &event, NULL, 0), 0,
                  "io_pgetevents returns 0 when no completions are queued");
        CHECK_ERR(syscall(SYS_io_pgetevents, ctx, 2, 1, &event, NULL, 0), EINVAL,
                  "io_pgetevents rejects min_nr greater than nr");

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

        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy io_pgetevents context");
    }

    CHECK_ERR(syscall(SYS_io_pgetevents, 0, 0, 1, NULL, NULL, 0), EINVAL,
              "io_pgetevents rejects invalid context");
    TEST_DONE();
}
