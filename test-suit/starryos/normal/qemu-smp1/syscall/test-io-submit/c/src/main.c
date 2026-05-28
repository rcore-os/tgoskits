#include "test_framework.h"

#include <fcntl.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_io_submit"
#define IOCB_CMD_PREAD 0
#define IOCB_CMD_PWRITE 1
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

static int create_fixture(void)
{
    int fd = open(TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) {
        return -1;
    }
    if (write(fd, "0123456789abcdef", 16) != 16) {
        close(fd);
        return -1;
    }
    return fd;
}

int main(void)
{
    TEST_START("io_submit syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 4, &ctx), 0,
              "create context for io_submit");

    int fd = create_fixture();
    CHECK(fd >= 0, "create io_submit fixture");
    if (ctx != 0 && fd >= 0) {
        char buf[8] = {0};
        struct iocb cb;
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x1111;
        cb.aio_lio_opcode = IOCB_CMD_PREAD;
        cb.aio_fildes = (uint32_t)fd;
        cb.aio_buf = (uint64_t)(uintptr_t)buf;
        cb.aio_nbytes = 4;
        cb.aio_offset = 5;
        struct iocb *list[1] = {&cb};

        CHECK_RET(syscall(SYS_io_submit, ctx, 1, list), 1,
                  "io_submit accepts one pread iocb");
        CHECK(memcmp(buf, "5678", 4) == 0,
              "io_submit executes supported pread operation synchronously");

        struct io_event event;
        memset(&event, 0, sizeof(event));
        CHECK_RET(syscall(SYS_io_getevents, ctx, 1, 1, &event, NULL), 1,
                  "io_getevents drains submitted completion");
        CHECK(event.data == 0x1111, "completion preserves iocb data");
        CHECK(event.obj == (uint64_t)(uintptr_t)&cb, "completion preserves iocb pointer");
        CHECK(event.res == 4 && event.res2 == 0, "completion reports pread byte count");

        const char write_data[] = "WXYZ";
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x2222;
        cb.aio_lio_opcode = IOCB_CMD_PWRITE;
        cb.aio_fildes = (uint32_t)fd;
        cb.aio_buf = (uint64_t)(uintptr_t)write_data;
        cb.aio_nbytes = 4;
        cb.aio_offset = 8;
        list[0] = &cb;

        CHECK_RET(lseek(fd, 1, SEEK_SET), 1, "set current offset before aio pwrite");
        CHECK_RET(syscall(SYS_io_submit, ctx, 1, list), 1,
                  "io_submit accepts one pwrite iocb");
        memset(&event, 0, sizeof(event));
        CHECK_RET(syscall(SYS_io_getevents, ctx, 1, 1, &event, NULL), 1,
                  "io_getevents drains pwrite completion");
        CHECK(event.data == 0x2222, "pwrite completion preserves iocb data");
        CHECK(event.res == 4 && event.res2 == 0, "completion reports pwrite byte count");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), 1,
                  "aio pwrite does not change current file offset");
        memset(buf, 0, sizeof(buf));
        CHECK_RET(syscall(SYS_pread64, fd, buf, 4, 8), 4,
                  "read back aio pwrite bytes");
        CHECK(memcmp(buf, "WXYZ", 4) == 0,
              "io_submit pwrite updates the requested range");

        memset(&cb, 0, sizeof(cb));
        cb.aio_lio_opcode = IOCB_CMD_NOOP;
        list[0] = &cb;
        CHECK_RET(syscall(SYS_io_submit, ctx, 0, list), 0,
                  "io_submit with nr 0 returns 0");
    }

    CHECK_ERR(syscall(SYS_io_submit, 0, 1, NULL), EINVAL,
              "io_submit rejects invalid context");
    CHECK_ERR(syscall(SYS_io_submit, ctx, -1, NULL), EINVAL,
              "io_submit rejects negative nr");

    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "close io_submit fixture");
    }
    if (ctx != 0) {
        CHECK_RET(syscall(SYS_io_destroy, ctx), 0,
                  "destroy io_submit context");
    }
    unlink(TMPFILE);
    TEST_DONE();
}
