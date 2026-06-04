#include "test_framework.h"

#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#define AIO_RING_MAGIC 0xa10a10a1u

typedef unsigned long aio_context_t;

struct aio_ring {
    uint32_t id;
    uint32_t nr;
    uint32_t head;
    uint32_t tail;
    uint32_t magic;
    uint32_t compat_features;
    uint32_t incompat_features;
    uint32_t header_length;
};

int main(void)
{
    TEST_START("io_setup syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "io_setup creates an AIO context");
    CHECK(ctx != 0, "io_setup stores a nonzero context id");
    if (ctx != 0) {
        struct aio_ring *ring = (struct aio_ring *)(uintptr_t)ctx;
        CHECK(ring->magic == AIO_RING_MAGIC,
              "io_setup context points to a readable AIO ring");
        CHECK(ring->nr >= 4, "AIO ring exposes at least requested events");
        CHECK(ring->head == 0 && ring->tail == 0,
              "new AIO ring starts empty");
        CHECK(ring->header_length == sizeof(struct aio_ring),
              "AIO ring header length matches userspace layout");
    }
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
