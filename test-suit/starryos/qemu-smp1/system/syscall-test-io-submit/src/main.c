#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#define TMPFILE "/tmp/starry_test_io_submit"
#define IOCB_CMD_PREAD 0
#define IOCB_CMD_PWRITE 1
#define IOCB_CMD_FSYNC 2
#define IOCB_CMD_FDSYNC 3
#define IOCB_CMD_POLL 5
#define IOCB_CMD_NOOP 6
#define IOCB_CMD_PREADV 7
#define IOCB_CMD_PWRITEV 8
#define IOCB_FLAG_RESFD 1u

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

static struct io_event wait_one(aio_context_t ctx, const char *msg)
{
    struct io_event event;
    memset(&event, 0, sizeof(event));
    CHECK_RET(syscall(SYS_io_getevents, ctx, 1, 1, &event, NULL), 1, msg);
    return event;
}

static void submit_one(aio_context_t ctx, struct iocb *cb, const char *msg)
{
    struct iocb *list[1] = {cb};
    CHECK_RET(syscall(SYS_io_submit, ctx, 1, list), 1, msg);
}

int main(void)
{
    TEST_START("io_submit syscall semantics");

    aio_context_t ctx = 0;
    CHECK_RET(syscall(SYS_io_setup, 8, &ctx), 0,
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

        submit_one(ctx, &cb, "io_submit accepts one pread iocb");
        struct io_event event = wait_one(ctx, "io_getevents drains pread completion");
        CHECK(memcmp(buf, "5678", 4) == 0,
              "io_submit pread fills the requested user buffer");
        CHECK(event.data == 0x1111, "pread completion preserves iocb data");
        CHECK(event.obj == (uint64_t)(uintptr_t)&cb,
              "pread completion preserves iocb pointer");
        CHECK(event.res == 4 && event.res2 == 0,
              "completion reports pread byte count");

        const char write_data[] = "WXYZ";
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x2222;
        cb.aio_lio_opcode = IOCB_CMD_PWRITE;
        cb.aio_fildes = (uint32_t)fd;
        cb.aio_buf = (uint64_t)(uintptr_t)write_data;
        cb.aio_nbytes = 4;
        cb.aio_offset = 8;

        CHECK_RET(lseek(fd, 1, SEEK_SET), 1,
                  "set current offset before aio pwrite");
        submit_one(ctx, &cb, "io_submit accepts one pwrite iocb");
        event = wait_one(ctx, "io_getevents drains pwrite completion");
        CHECK(event.data == 0x2222, "pwrite completion preserves iocb data");
        CHECK(event.res == 4 && event.res2 == 0,
              "completion reports pwrite byte count");
        CHECK_RET(lseek(fd, 0, SEEK_CUR), 1,
                  "aio pwrite does not change current file offset");
        memset(buf, 0, sizeof(buf));
        CHECK_RET(syscall(SYS_pread64, fd, buf, 4, 8), 4,
                  "read back aio pwrite bytes");
        CHECK(memcmp(buf, "WXYZ", 4) == 0,
              "io_submit pwrite updates the requested range");

        char left[3] = {0};
        char right[3] = {0};
        struct iovec read_iov[2] = {
            {.iov_base = left, .iov_len = 2},
            {.iov_base = right, .iov_len = 2},
        };
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x3333;
        cb.aio_lio_opcode = IOCB_CMD_PREADV;
        cb.aio_fildes = (uint32_t)fd;
        cb.aio_buf = (uint64_t)(uintptr_t)read_iov;
        cb.aio_nbytes = 2;
        cb.aio_offset = 0;

        submit_one(ctx, &cb, "io_submit accepts one preadv iocb");
        event = wait_one(ctx, "io_getevents drains preadv completion");
        CHECK(event.data == 0x3333 && event.res == 4,
              "preadv completion reports total byte count");
        CHECK(memcmp(left, "01", 2) == 0 && memcmp(right, "23", 2) == 0,
              "preadv scatters bytes into both iov segments");

        const char vec_a[] = "AB";
        const char vec_b[] = "CD";
        struct iovec write_iov[2] = {
            {.iov_base = (void *)vec_a, .iov_len = 2},
            {.iov_base = (void *)vec_b, .iov_len = 2},
        };
        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x4444;
        cb.aio_lio_opcode = IOCB_CMD_PWRITEV;
        cb.aio_fildes = (uint32_t)fd;
        cb.aio_buf = (uint64_t)(uintptr_t)write_iov;
        cb.aio_nbytes = 2;
        cb.aio_offset = 12;

        submit_one(ctx, &cb, "io_submit accepts one pwritev iocb");
        event = wait_one(ctx, "io_getevents drains pwritev completion");
        CHECK(event.data == 0x4444 && event.res == 4,
              "pwritev completion reports total byte count");
        memset(buf, 0, sizeof(buf));
        CHECK_RET(syscall(SYS_pread64, fd, buf, 4, 12), 4,
                  "read back aio pwritev bytes");
        CHECK(memcmp(buf, "ABCD", 4) == 0,
              "pwritev gathers bytes from both iov segments");

        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x5555;
        cb.aio_lio_opcode = IOCB_CMD_FSYNC;
        cb.aio_fildes = (uint32_t)fd;
        submit_one(ctx, &cb, "io_submit accepts fsync iocb");
        event = wait_one(ctx, "io_getevents drains fsync completion");
        CHECK(event.data == 0x5555 && event.res == 0 && event.res2 == 0,
              "fsync completion reports success");

        memset(&cb, 0, sizeof(cb));
        cb.aio_data = 0x6666;
        cb.aio_lio_opcode = IOCB_CMD_FDSYNC;
        cb.aio_fildes = (uint32_t)fd;
        submit_one(ctx, &cb, "io_submit accepts fdatasync iocb");
        event = wait_one(ctx, "io_getevents drains fdatasync completion");
        CHECK(event.data == 0x6666 && event.res == 0 && event.res2 == 0,
              "fdatasync completion reports success");

        int pipefd[2] = {-1, -1};
        CHECK_RET(pipe(pipefd), 0, "create pipe for aio poll");
        if (pipefd[0] >= 0) {
            CHECK_RET(write(pipefd[1], "P", 1), 1,
                      "make pipe readable before aio poll");
            memset(&cb, 0, sizeof(cb));
            cb.aio_data = 0x7777;
            cb.aio_lio_opcode = IOCB_CMD_POLL;
            cb.aio_fildes = (uint32_t)pipefd[0];
            cb.aio_buf = POLLIN;
            submit_one(ctx, &cb, "io_submit accepts poll iocb");
            event = wait_one(ctx, "io_getevents drains poll completion");
            CHECK(event.data == 0x7777 && (event.res & POLLIN) != 0,
                  "poll completion reports readable pipe");
            CHECK_RET(close(pipefd[0]), 0, "close aio poll read end");
            CHECK_RET(close(pipefd[1]), 0, "close aio poll write end");
        }

        int efd = eventfd(0, EFD_NONBLOCK);
        CHECK(efd >= 0, "create eventfd for AIO resfd notification");
        if (efd >= 0) {
            uint64_t counter = 0;
            memset(&cb, 0, sizeof(cb));
            cb.aio_data = 0x8888;
            cb.aio_lio_opcode = IOCB_CMD_NOOP;
            cb.aio_flags = IOCB_FLAG_RESFD;
            cb.aio_resfd = (uint32_t)efd;
            submit_one(ctx, &cb, "io_submit accepts noop with resfd");
            event = wait_one(ctx, "io_getevents drains resfd noop completion");
            CHECK(event.data == 0x8888 && event.res == 0,
                  "noop completion with resfd reports success");
            CHECK_RET(read(efd, &counter, sizeof(counter)), (ssize_t)sizeof(counter),
                      "eventfd receives one AIO completion notification");
            CHECK(counter == 1, "eventfd notification counter is one");
            CHECK_RET(close(efd), 0, "close aio eventfd");
        }

        memset(&cb, 0, sizeof(cb));
        cb.aio_lio_opcode = IOCB_CMD_NOOP;
        cb.aio_flags = IOCB_FLAG_RESFD;
        cb.aio_resfd = (uint32_t)fd;
        struct iocb *bad_resfd[1] = {&cb};
        CHECK_ERR(syscall(SYS_io_submit, ctx, 1, bad_resfd), EINVAL,
                  "io_submit rejects IOCB_FLAG_RESFD with non-eventfd fd");

        struct iocb valid;
        struct iocb invalid;
        memset(&valid, 0, sizeof(valid));
        memset(&invalid, 0, sizeof(invalid));
        valid.aio_data = 0x9999;
        valid.aio_lio_opcode = IOCB_CMD_NOOP;
        invalid.aio_lio_opcode = 0xffff;
        struct iocb *batch[2] = {&valid, &invalid};
        CHECK_RET(syscall(SYS_io_submit, ctx, 2, batch), 1,
                  "io_submit returns partial count after first queued iocb");
        event = wait_one(ctx, "io_getevents drains partial-submit completion");
        CHECK(event.data == 0x9999 && event.obj == (uint64_t)(uintptr_t)&valid,
              "partial-submit completion belongs to first iocb");

        struct iocb *single[1] = {&invalid};
        CHECK_ERR(syscall(SYS_io_submit, ctx, 1, single), EINVAL,
                  "io_submit rejects unsupported opcode without partial success");
        invalid.aio_lio_opcode = IOCB_CMD_NOOP;
        invalid.aio_reserved2 = 1;
        CHECK_ERR(syscall(SYS_io_submit, ctx, 1, single), EINVAL,
                  "io_submit rejects nonzero reserved field");
        invalid.aio_reserved2 = 0;
        invalid.aio_flags = 0x80000000u;
        CHECK_ERR(syscall(SYS_io_submit, ctx, 1, single), EINVAL,
                  "io_submit rejects unknown iocb flags");

        CHECK_RET(syscall(SYS_io_submit, ctx, 0, single), 0,
                  "io_submit with nr 0 returns 0");
    }

    CHECK_ERR(syscall(SYS_io_submit, 0, 1, NULL), EINVAL,
              "io_submit rejects invalid context");
    CHECK_ERR(syscall(SYS_io_submit, ctx, (long)-1, NULL), EINVAL,
              "io_submit rejects negative nr");
    CHECK_ERR(syscall(SYS_io_submit, ctx, 1, (void *)(uintptr_t)1), EFAULT,
              "io_submit rejects invalid iocb pointer array");

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
