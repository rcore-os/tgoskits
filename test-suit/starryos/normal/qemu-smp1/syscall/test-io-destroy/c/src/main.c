#include "test_framework.h"

#include <sys/syscall.h>
#include <unistd.h>

typedef unsigned long aio_context_t;

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

    CHECK_ERR(syscall(SYS_io_destroy, 0), EINVAL,
              "io_destroy rejects context 0");
    CHECK_ERR(syscall(SYS_io_destroy, 0x7fffffffUL), EINVAL,
              "io_destroy rejects an unknown context id");

    TEST_DONE();
}
