#include "test_framework.h"

#include <sys/syscall.h>
#include <unistd.h>

typedef unsigned long aio_context_t;

int main(void)
{
    TEST_START("io_setup syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "io_setup creates an AIO context");
    CHECK(ctx != 0, "io_setup stores a nonzero context id");
    if (ctx != 0) {
        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy context created by io_setup");
    }

    ctx = 0;
    CHECK_ERR(syscall(SYS_io_setup, 0, &ctx), EINVAL,
              "io_setup rejects nr_events 0");
    CHECK(ctx == 0, "failed io_setup leaves zero context unchanged");

    ctx = 0x1234;
    CHECK_ERR(syscall(SYS_io_setup, 4, &ctx), EINVAL,
              "io_setup rejects nonzero user context slot");
    CHECK(ctx == 0x1234, "failed io_setup leaves nonzero context unchanged");

    TEST_DONE();
}
