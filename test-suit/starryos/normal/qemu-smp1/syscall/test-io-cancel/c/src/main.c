#include "test_framework.h"

#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

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
    CHECK_RET(syscall(SYS_io_setup, 2, &ctx), 0,
              "create context for io_cancel");

    if (ctx != 0) {
        struct iocb cb;
        struct io_event event;
        memset(&cb, 0, sizeof(cb));
        memset(&event, 0, sizeof(event));

        CHECK_ERR(syscall(SYS_io_cancel, ctx, &cb, &event), EINVAL,
                  "io_cancel reports unavailable for completed operations");
        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy io_cancel context");
    }

    CHECK_ERR(syscall(SYS_io_cancel, 0, NULL, NULL), EINVAL,
              "io_cancel rejects invalid context");
    TEST_DONE();
}
