#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef SYS_io_uring_setup
#define SYS_io_uring_setup 425
#endif
#ifndef SYS_io_uring_enter
#define SYS_io_uring_enter 426
#endif
#ifndef SYS_io_uring_register
#define SYS_io_uring_register 427
#endif

#ifndef MAP_POPULATE
#define MAP_POPULATE 0x8000
#endif

#define IORING_OFF_SQ_RING 0ULL
#define IORING_OFF_CQ_RING 0x08000000ULL
#define IORING_OFF_SQES 0x10000000ULL

#define IORING_ENTER_GETEVENTS 1U
#define IORING_REGISTER_PROBE 8U

#define IORING_OP_NOP 0U
#define IORING_OP_READV 1U
#define IORING_OP_WRITEV 2U
#define IORING_OP_FSYNC 3U
#define IORING_OP_READ 22U
#define IORING_OP_WRITE 23U
#define IORING_OP_LAST 63U

struct io_sqring_offsets {
    uint32_t head;
    uint32_t tail;
    uint32_t ring_mask;
    uint32_t ring_entries;
    uint32_t flags;
    uint32_t dropped;
    uint32_t array;
    uint32_t resv1;
    uint64_t user_addr;
};

struct io_cqring_offsets {
    uint32_t head;
    uint32_t tail;
    uint32_t ring_mask;
    uint32_t ring_entries;
    uint32_t overflow;
    uint32_t cqes;
    uint32_t flags;
    uint32_t resv1;
    uint64_t user_addr;
};

struct io_uring_params {
    uint32_t sq_entries;
    uint32_t cq_entries;
    uint32_t flags;
    uint32_t sq_thread_cpu;
    uint32_t sq_thread_idle;
    uint32_t features;
    uint32_t wq_fd;
    uint32_t resv[3];
    struct io_sqring_offsets sq_off;
    struct io_cqring_offsets cq_off;
};

struct io_uring_sqe {
    uint8_t opcode;
    uint8_t flags;
    uint16_t ioprio;
    int32_t fd;
    uint64_t off;
    uint64_t addr;
    uint32_t len;
    uint32_t rw_flags;
    uint64_t user_data;
    uint16_t buf_index;
    uint16_t personality;
    int32_t splice_fd_in;
    uint64_t addr3;
    uint64_t pad2;
};

struct io_uring_cqe {
    uint64_t user_data;
    int32_t res;
    uint32_t flags;
};

struct io_uring_probe_op {
    uint8_t op;
    uint8_t resv;
    uint16_t flags;
    uint32_t resv2;
};

struct io_uring_probe {
    uint8_t last_op;
    uint8_t ops_len;
    uint16_t resv;
    uint32_t resv2[3];
    struct io_uring_probe_op ops[8];
};

struct ring_view {
    int fd;
    struct io_uring_params params;
    volatile uint32_t *sq_head;
    volatile uint32_t *sq_tail;
    volatile uint32_t *sq_mask;
    volatile uint32_t *sq_entries;
    volatile uint32_t *sq_array;
    volatile uint32_t *cq_head;
    volatile uint32_t *cq_tail;
    volatile uint32_t *cq_mask;
    volatile uint32_t *cq_entries;
    struct io_uring_sqe *sqes;
    struct io_uring_cqe *cqes;
    size_t sq_ring_sz;
    size_t cq_ring_sz;
    size_t sqes_sz;
    void *sq_ring;
    void *cq_ring;
};

static int is_power_of_two(uint32_t value)
{
    return value != 0 && (value & (value - 1)) == 0;
}

static void *checked_mmap(int fd, size_t len, off_t offset, const char *name)
{
    void *addr = mmap(NULL, len, PROT_READ | PROT_WRITE,
                      MAP_SHARED | MAP_POPULATE, fd, offset);
    CHECK(addr != MAP_FAILED, name);
    return addr;
}

static int ring_open(struct ring_view *ring)
{
    memset(ring, 0, sizeof(*ring));
    ring->fd = syscall(SYS_io_uring_setup, 8, &ring->params);
    CHECK(ring->fd >= 0, "io_uring_setup creates a ring fd");
    if (ring->fd < 0) {
        return -1;
    }

    CHECK(ring->params.sq_entries >= 8 && is_power_of_two(ring->params.sq_entries),
          "setup returns a power-of-two SQ size");
    CHECK(ring->params.cq_entries >= ring->params.sq_entries &&
              is_power_of_two(ring->params.cq_entries),
          "setup returns a usable CQ size");

    ring->sq_ring_sz = ring->params.sq_off.array +
                       ring->params.sq_entries * sizeof(uint32_t);
    ring->cq_ring_sz = ring->params.cq_off.cqes +
                       ring->params.cq_entries * sizeof(struct io_uring_cqe);
    ring->sqes_sz = ring->params.sq_entries * sizeof(struct io_uring_sqe);

    ring->sq_ring = checked_mmap(ring->fd, ring->sq_ring_sz,
                                 IORING_OFF_SQ_RING, "mmap SQ ring");
    ring->cq_ring = checked_mmap(ring->fd, ring->cq_ring_sz,
                                 IORING_OFF_CQ_RING, "mmap CQ ring");
    ring->sqes = checked_mmap(ring->fd, ring->sqes_sz,
                              IORING_OFF_SQES, "mmap SQEs");
    if (ring->sq_ring == MAP_FAILED || ring->cq_ring == MAP_FAILED ||
        ring->sqes == MAP_FAILED) {
        return -1;
    }

    ring->sq_head = (volatile uint32_t *)((char *)ring->sq_ring +
                                          ring->params.sq_off.head);
    ring->sq_tail = (volatile uint32_t *)((char *)ring->sq_ring +
                                          ring->params.sq_off.tail);
    ring->sq_mask = (volatile uint32_t *)((char *)ring->sq_ring +
                                          ring->params.sq_off.ring_mask);
    ring->sq_entries = (volatile uint32_t *)((char *)ring->sq_ring +
                                             ring->params.sq_off.ring_entries);
    ring->sq_array = (volatile uint32_t *)((char *)ring->sq_ring +
                                           ring->params.sq_off.array);
    ring->cq_head = (volatile uint32_t *)((char *)ring->cq_ring +
                                          ring->params.cq_off.head);
    ring->cq_tail = (volatile uint32_t *)((char *)ring->cq_ring +
                                          ring->params.cq_off.tail);
    ring->cq_mask = (volatile uint32_t *)((char *)ring->cq_ring +
                                          ring->params.cq_off.ring_mask);
    ring->cq_entries = (volatile uint32_t *)((char *)ring->cq_ring +
                                             ring->params.cq_off.ring_entries);
    ring->cqes = (struct io_uring_cqe *)((char *)ring->cq_ring +
                                         ring->params.cq_off.cqes);

    CHECK(*ring->sq_mask == ring->params.sq_entries - 1,
          "SQ ring mask matches entries");
    CHECK(*ring->cq_mask == ring->params.cq_entries - 1,
          "CQ ring mask matches entries");
    CHECK(*ring->sq_entries == ring->params.sq_entries,
          "SQ ring exposes entry count");
    CHECK(*ring->cq_entries == ring->params.cq_entries,
          "CQ ring exposes entry count");
    return 0;
}

static void ring_close(struct ring_view *ring)
{
    if (ring->sq_ring && ring->sq_ring != MAP_FAILED) {
        munmap(ring->sq_ring, ring->sq_ring_sz);
    }
    if (ring->cq_ring && ring->cq_ring != MAP_FAILED) {
        munmap(ring->cq_ring, ring->cq_ring_sz);
    }
    if (ring->sqes && ring->sqes != MAP_FAILED) {
        munmap(ring->sqes, ring->sqes_sz);
    }
    if (ring->fd >= 0) {
        close(ring->fd);
    }
}

static struct io_uring_cqe submit_one(struct ring_view *ring,
                                      const struct io_uring_sqe *template,
                                      uint64_t user_data)
{
    uint32_t tail = *ring->sq_tail;
    uint32_t slot = tail & *ring->sq_mask;
    uint32_t sqe_index = slot;
    struct io_uring_sqe *sqe = &ring->sqes[sqe_index];

    memset(sqe, 0, sizeof(*sqe));
    *sqe = *template;
    sqe->user_data = user_data;
    ring->sq_array[slot] = sqe_index;
    *ring->sq_tail = tail + 1;

    CHECK_RET(syscall(SYS_io_uring_enter, ring->fd, 1, 1,
                      IORING_ENTER_GETEVENTS, NULL, 0),
              1, "io_uring_enter submits one SQE");
    CHECK(*ring->cq_tail != *ring->cq_head,
          "io_uring_enter posts one CQE");

    uint32_t cq_slot = *ring->cq_head & *ring->cq_mask;
    struct io_uring_cqe cqe = ring->cqes[cq_slot];
    *ring->cq_head = *ring->cq_head + 1;
    CHECK(cqe.user_data == user_data, "CQE preserves user_data");
    return cqe;
}

int main(void)
{
    TEST_START("io_uring lite syscall semantics");

    struct io_uring_params bad_params;
    memset(&bad_params, 0, sizeof(bad_params));
    CHECK_ERR(syscall(SYS_io_uring_setup, 0, &bad_params), EINVAL,
              "io_uring_setup rejects zero entries");

    struct ring_view ring;
    if (ring_open(&ring) != 0) {
        TEST_DONE();
    }

    struct io_uring_probe probe;
    memset(&probe, 0, sizeof(probe));
    CHECK_RET(syscall(SYS_io_uring_register, ring.fd, IORING_REGISTER_PROBE,
                      &probe, 8),
              0, "io_uring_register PROBE succeeds");
    CHECK(probe.last_op >= IORING_OP_WRITE && probe.ops_len > 0,
          "probe reports supported operations");
    CHECK(probe.ops[0].op == IORING_OP_NOP, "probe includes NOP first");

    struct io_uring_sqe sqe;
    memset(&sqe, 0, sizeof(sqe));
    sqe.opcode = IORING_OP_NOP;
    struct io_uring_cqe cqe = submit_one(&ring, &sqe, 0x1001);
    CHECK(cqe.res == 0, "NOP completes with res=0");

    const char *path = "/tmp/starry-io-uring-lite.txt";
    int file = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    CHECK(file >= 0, "open backing file for io_uring read/write");
    if (file >= 0) {
        struct iovec wiov[2];
        wiov[0].iov_base = (void *)"ab";
        wiov[0].iov_len = 2;
        wiov[1].iov_base = (void *)"cd";
        wiov[1].iov_len = 2;

        memset(&sqe, 0, sizeof(sqe));
        sqe.opcode = IORING_OP_WRITEV;
        sqe.fd = file;
        sqe.addr = (uint64_t)(uintptr_t)wiov;
        sqe.len = 2;
        sqe.off = 0;
        cqe = submit_one(&ring, &sqe, 0x2001);
        CHECK(cqe.res == 4, "WRITEV writes both iovecs");

        char r1[3] = {0};
        char r2[3] = {0};
        struct iovec riov[2];
        riov[0].iov_base = r1;
        riov[0].iov_len = 2;
        riov[1].iov_base = r2;
        riov[1].iov_len = 2;

        memset(&sqe, 0, sizeof(sqe));
        sqe.opcode = IORING_OP_READV;
        sqe.fd = file;
        sqe.addr = (uint64_t)(uintptr_t)riov;
        sqe.len = 2;
        sqe.off = 0;
        cqe = submit_one(&ring, &sqe, 0x2002);
        CHECK(cqe.res == 4, "READV reads both iovecs");
        CHECK(strcmp(r1, "ab") == 0 && strcmp(r2, "cd") == 0,
              "READV returns expected data");

        char extra[] = "EF";
        memset(&sqe, 0, sizeof(sqe));
        sqe.opcode = IORING_OP_WRITE;
        sqe.fd = file;
        sqe.addr = (uint64_t)(uintptr_t)extra;
        sqe.len = 2;
        sqe.off = 4;
        cqe = submit_one(&ring, &sqe, 0x2003);
        CHECK(cqe.res == 2, "WRITE writes a fixed buffer");

        char all[7] = {0};
        memset(&sqe, 0, sizeof(sqe));
        sqe.opcode = IORING_OP_READ;
        sqe.fd = file;
        sqe.addr = (uint64_t)(uintptr_t)all;
        sqe.len = 6;
        sqe.off = 0;
        cqe = submit_one(&ring, &sqe, 0x2004);
        CHECK(cqe.res == 6, "READ reads a fixed buffer");
        CHECK(strcmp(all, "abcdEF") == 0, "READ returns combined data");

        memset(&sqe, 0, sizeof(sqe));
        sqe.opcode = IORING_OP_FSYNC;
        sqe.fd = file;
        cqe = submit_one(&ring, &sqe, 0x2005);
        CHECK(cqe.res == 0, "FSYNC completes successfully");

        close(file);
        unlink(path);
    }

    memset(&sqe, 0, sizeof(sqe));
    sqe.opcode = IORING_OP_LAST;
    cqe = submit_one(&ring, &sqe, 0x3001);
    CHECK(cqe.res == -EOPNOTSUPP, "unsupported opcode completes with -EOPNOTSUPP");

    ring_close(&ring);
    TEST_DONE();
}
