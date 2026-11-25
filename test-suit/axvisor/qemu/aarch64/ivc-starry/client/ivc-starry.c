/*
 * One freestanding AArch64 program for both StarryOS AXIVC peers.
 *
 * The program opens only /dev/axivc0. The kernel selects the unique ivshmem
 * function and derives the publisher/subscriber role from the PCI peer ID, so
 * userspace supplies no VM id, channel key, channel size or PCI address.
 */

typedef unsigned long u64;

#define AT_FDCWD (-100)
#define O_WRONLY 01
#define O_RDWR 02
#define O_NONBLOCK 04000

#define SYS_IOCTL 29
#define SYS_OPENAT 56
#define SYS_CLOSE 57
#define SYS_READ 63
#define SYS_WRITE 64
#define SYS_PPOLL 73
#define SYS_MMAP 222
#define SYS_MUNMAP 215
#define SYS_MPROTECT 226

#define EAGAIN 11
#define EINTR 4
#define EACCES 13
#define EIO 5
#define EBUSY 16
#define ENODEV 19
#define EINVAL 22
#define ENOTTY 25

#define POLLIN 0x001
#define POLLOUT 0x004
#define POLLERR 0x008
#define POLLHUP 0x010
#define POLLNVAL 0x020

#define PROT_READ 0x1
#define PROT_WRITE 0x2
#define PROT_EXEC 0x4
#define MAP_SHARED 0x01

#define IVC_CACHE_FLUSH 0x40100004UL
#define IVC_CACHE_INVALIDATE 0x40100005UL
/* _IOR('I', 6, struct payload_info): stable version-1, 40-byte output. */
#define IVC_PAYLOAD_INFO 0x80284906UL

#define PAGE_SIZE 4096UL
#define BULK_SIZE (10UL * 1024UL * 1024UL)
#define MESSAGE_SIZE 48UL
#define PUBLISHER_SEED 0x21U
#define SUBSCRIBER_SEED 0xa5U
#define FORCED_REQUEST_SEED 0x51U
#define FORCED_ACK_SEED 0x52U
#define FORCED_REQUEST_CHECKSUM 0x1111222233334444UL
#define FORCED_ACK_CHECKSUM 0x5555666677778888UL
#define WAIT_TIMEOUT_SECONDS 30
#define PEER_WAIT_TIMEOUT_SECONDS 60

struct payload_info {
    u64 version;
    u64 generation;
    u64 local_offset;
    u64 peer_offset;
    u64 payload_size;
};

struct ivc_frame {
    u64 length;
    u64 checksum;
    unsigned char tag[32];
};

_Static_assert(sizeof(struct ivc_frame) == MESSAGE_SIZE, "frame ABI must be 48 bytes");
_Static_assert(sizeof(struct payload_info) == 40, "payload-info ABI must be 40 bytes");

struct cache_op_arg {
    u64 addr;
    u64 size;
};

struct pollfd {
    int fd;
    short events;
    short revents;
};

struct timespec {
    long tv_sec;
    long tv_nsec;
};

static int console_fd = -1;

void *memset(void *destination, int value, __SIZE_TYPE__ count) {
    unsigned char *bytes = destination;
    for (__SIZE_TYPE__ index = 0; index < count; index++) {
        bytes[index] = (unsigned char)value;
    }
    return destination;
}

__asm__(
    ".global _start\n"
    ".type _start, %function\n"
    "_start:\n"
    "mov x0, sp\n"
    "bl init_main\n"
    "mov x0, #1\n"
    "mov x8, #94\n"
    "svc #0\n"
    "1:\n"
    "b 1b\n"
    ".size _start, .-_start\n"
);

static long syscall3(long number, long a0, long a1, long a2) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x8)
                     : "memory", "cc");
    return x0;
}

static long syscall5(long number, long a0, long a1, long a2, long a3, long a4) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8)
                     : "memory", "cc");
    return x0;
}

static long syscall6(
    long number,
    long a0,
    long a1,
    long a2,
    long a3,
    long a4,
    long a5
) {
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x5 __asm__("x5") = a5;
    register long x8 __asm__("x8") = number;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5), "r"(x8)
                     : "memory", "cc");
    return x0;
}

static unsigned long text_len(const char *text) {
    unsigned long len = 0;
    while (text[len] != '\0') {
        len++;
    }
    return len;
}

static int same_text(const char *left, const char *right) {
    while (*left != '\0' && *left == *right) {
        left++;
        right++;
    }
    return *left == *right;
}

static void write_all(const char *text) {
    unsigned long remaining = text_len(text);
    const char *cursor = text;
    while (remaining != 0) {
        long written = syscall3(
            SYS_WRITE,
            console_fd,
            (long)cursor,
            (long)remaining
        );
        if (written == -EINTR) {
            continue;
        }
        if (written <= 0) {
            return;
        }
        cursor += written;
        remaining -= (unsigned long)written;
    }
}

static void write_number(unsigned long value) {
    char digits[32];
    unsigned long count = 0;
    if (value == 0) {
        write_all("0");
        return;
    }
    while (value != 0) {
        digits[count++] = (char)('0' + value % 10);
        value /= 10;
    }
    while (count != 0) {
        char one[2] = {digits[--count], '\0'};
        write_all(one);
    }
}

__attribute__((noreturn)) static void park_pid1(void) {
    for (;;) {
        syscall5(SYS_PPOLL, 0, 0, 0, 0, 8);
    }
}

__attribute__((noreturn)) static void fail(
    const char *role,
    const char *stage,
    long error
) {
    write_all("AXIVC_STARRY FAIL role=");
    write_all(role);
    write_all(" stage=");
    write_all(stage);
    write_all(" errno=");
    write_number((unsigned long)(error < 0 ? -error : error));
    write_all("\n");
    park_pid1();
}

static void require_zero(long result, const char *role, const char *stage) {
    if (result != 0) {
        fail(role, stage, result < 0 ? result : -EINVAL);
    }
}

static void open_console(void) {
    long fd = syscall3(
        SYS_OPENAT,
        AT_FDCWD,
        (long)"/dev/ttyS0",
        O_WRONLY
    );
    if (fd < 0) {
        park_pid1();
    }
    console_fd = (int)fd;
}

static int open_auto_channel(const char *role) {
    long fd = syscall3(
        SYS_OPENAT,
        AT_FDCWD,
        (long)"/dev/axivc0",
        O_RDWR | O_NONBLOCK
    );
    if (fd < 0) {
        fail(role, "open-axivc0", fd);
    }
    return (int)fd;
}

static void expect_second_open_busy(const char *role) {
    long fd = syscall3(
        SYS_OPENAT,
        AT_FDCWD,
        (long)"/dev/axivc0",
        O_RDWR | O_NONBLOCK
    );
    if (fd != -EBUSY) {
        fail(role, "second-open-exclusive", fd < 0 ? fd : -EINVAL);
    }
}

static void memory_barrier(void) {
    __asm__ volatile("dmb ish" ::: "memory");
}

static void wait_for_events(
    int fd,
    short requested,
    short expected,
    short forbidden,
    long timeout_seconds,
    const char *role,
    const char *stage
) {
    struct pollfd poll = {
        .fd = fd,
        .events = requested,
        .revents = 0,
    };
    struct timespec timeout = {
        .tv_sec = timeout_seconds,
        .tv_nsec = 0,
    };
    long result = syscall5(
        SYS_PPOLL,
        (long)&poll,
        1,
        (long)&timeout,
        0,
        8
    );
    if (result < 0) {
        fail(role, stage, result);
    }
    if (result == 0) {
        fail(role, stage, -EAGAIN);
    }
    if (result != 1) {
        fail(role, stage, -EINVAL);
    }
    if ((poll.revents & POLLNVAL) != 0) {
        fail(role, stage, -ENODEV);
    }
    if ((poll.revents & forbidden) != 0) {
        fail(role, stage, -EINVAL);
    }
    if ((poll.revents & expected) != expected) {
        fail(role, stage, -EINVAL);
    }
}

static void wait_for_terminal(
    int fd,
    short requested,
    const char *role,
    const char *stage
) {
    struct pollfd poll = {
        .fd = fd,
        .events = requested,
        .revents = 0,
    };
    struct timespec timeout = {
        .tv_sec = PEER_WAIT_TIMEOUT_SECONDS,
        .tv_nsec = 0,
    };
    long result = syscall5(
        SYS_PPOLL,
        (long)&poll,
        1,
        (long)&timeout,
        0,
        8
    );
    if (result != 1) {
        fail(role, stage, result < 0 ? result : -EAGAIN);
    }
    if ((poll.revents & POLLNVAL) != 0) {
        fail(role, stage, -ENODEV);
    }
    if ((poll.revents & POLLHUP) == 0) {
        fail(role, stage, -EINVAL);
    }
}

static void expect_empty_read(int fd, const char *role, const char *stage) {
    unsigned char buffer[MESSAGE_SIZE];
    long result = syscall3(SYS_READ, fd, (long)buffer, MESSAGE_SIZE);
    if (result != -EAGAIN) {
        fail(role, stage, result);
    }
}

static void expect_negative(long result, const char *role, const char *stage) {
    if (result >= 0) {
        fail(role, stage, -EINVAL);
    }
}

static void expect_error(
    long result,
    long expected,
    const char *role,
    const char *stage
) {
    if (result != expected) {
        fail(role, stage, result < 0 ? result : -EINVAL);
    }
}

static void *map_payload(
    int fd,
    u64 offset,
    u64 length,
    long protection,
    const char *role,
    const char *stage
) {
    long mapping = syscall6(
        SYS_MMAP,
        0,
        (long)length,
        protection,
        MAP_SHARED,
        fd,
        (long)offset
    );
    if (mapping < 0) {
        fail(role, stage, mapping);
    }
    return (void *)mapping;
}

static void unmap_payload(
    void *mapping,
    u64 length,
    const char *role,
    const char *stage
) {
    require_zero(
        syscall3(SYS_MUNMAP, (long)mapping, (long)length, 0),
        role,
        stage
    );
}

static void test_payload_contract(
    int channel,
    void **local_mapping,
    void **peer_mapping,
    const char *role
) {
    struct payload_info info = {0};
    struct cache_op_arg cache = {
        .addr = 0,
        .size = 0,
    };
    require_zero(
        syscall3(SYS_IOCTL, channel, IVC_PAYLOAD_INFO, (long)&info),
        role,
        "payload-info"
    );
    if (info.version != 1
        || info.generation == 0
        || info.local_offset != 0
        || info.peer_offset == 0
        || info.peer_offset == info.local_offset
        || info.peer_offset % PAGE_SIZE != 0
        || info.payload_size < BULK_SIZE) {
        fail(role, "payload-geometry", -EINVAL);
    }

    long flush = syscall3(
        SYS_IOCTL,
        channel,
        IVC_CACHE_FLUSH,
        (long)&cache
    );
    if (flush != -ENOTTY) {
        fail(role, "cache-flush", flush);
    }
    long invalidate = syscall3(
        SYS_IOCTL,
        channel,
        IVC_CACHE_INVALIDATE,
        (long)&cache
    );
    if (invalidate != -ENOTTY) {
        fail(role, "cache-invalidate", invalidate);
    }

    *local_mapping = map_payload(
        channel,
        info.local_offset,
        BULK_SIZE,
        PROT_READ | PROT_WRITE,
        role,
        "map-local-payload"
    );
    *peer_mapping = map_payload(
        channel,
        info.peer_offset,
        BULK_SIZE,
        PROT_READ,
        role,
        "map-peer-payload"
    );

    expect_error(
        syscall6(
            SYS_MMAP,
            0,
            BULK_SIZE,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            channel,
            (long)info.peer_offset
        ),
        -EACCES,
        role,
        "peer-writable-mmap"
    );
    expect_error(
        syscall6(
            SYS_MMAP,
            0,
            PAGE_SIZE,
            PROT_READ | PROT_EXEC,
            MAP_SHARED,
            channel,
            (long)info.local_offset
        ),
        -EACCES,
        role,
        "local-exec-mmap"
    );
    expect_error(
        syscall6(
            SYS_MMAP,
            0,
            PAGE_SIZE,
            PROT_READ | PROT_EXEC,
            MAP_SHARED,
            channel,
            (long)info.peer_offset
        ),
        -EACCES,
        role,
        "peer-exec-mmap"
    );
    expect_negative(
        syscall6(
            SYS_MMAP,
            0,
            info.payload_size + PAGE_SIZE,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            channel,
            (long)info.local_offset
        ),
        role,
        "oversize-mmap"
    );
    expect_negative(
        syscall6(
            SYS_MMAP,
            0,
            PAGE_SIZE,
            PROT_READ,
            MAP_SHARED,
            channel,
            PAGE_SIZE
        ),
        role,
        "protocol-page-mmap"
    );
    expect_negative(
        syscall6(
            SYS_MMAP,
            0,
            PAGE_SIZE,
            PROT_READ,
            MAP_SHARED,
            channel,
            (long)(info.peer_offset + PAGE_SIZE)
        ),
        role,
        "bad-offset-mmap"
    );
    expect_error(
        syscall3(
            SYS_MPROTECT,
            (long)*peer_mapping,
            BULK_SIZE,
            PROT_READ | PROT_WRITE
        ),
        -EACCES,
        role,
        "peer-mprotect-write"
    );
    expect_error(
        syscall3(
            SYS_MPROTECT,
            (long)*local_mapping,
            PAGE_SIZE,
            PROT_READ | PROT_EXEC
        ),
        -EACCES,
        role,
        "local-mprotect-exec"
    );
}

static void fill_payload(unsigned char *payload, unsigned int seed) {
    for (u64 index = 0; index < BULK_SIZE; index++) {
        payload[index] = (unsigned char)(seed + index * 13U);
    }
}

static u64 payload_checksum(const unsigned char *payload) {
    u64 checksum = 0xcbf29ce484222325UL;
    for (u64 index = 0; index < BULK_SIZE; index++) {
        checksum = (checksum << 5) - checksum + payload[index];
    }
    return checksum;
}

static void fill_frame(
    struct ivc_frame *frame,
    u64 length,
    u64 checksum,
    unsigned int seed
) {
    frame->length = length;
    frame->checksum = checksum;
    for (unsigned int index = 0; index < 32; index++) {
        frame->tag[index] = (unsigned char)(seed + index * 7U);
    }
}

static int frame_matches(
    const struct ivc_frame *frame,
    u64 length,
    unsigned int seed
) {
    if (frame->length != length) {
        return 0;
    }
    for (unsigned int index = 0; index < 32; index++) {
        if (frame->tag[index] != (unsigned char)(seed + index * 7U)) {
            return 0;
        }
    }
    return 1;
}

static void write_frame(
    int channel,
    const struct ivc_frame *frame,
    const char *role,
    const char *stage
) {
    memory_barrier();
    long written = syscall3(
        SYS_WRITE,
        channel,
        (long)frame,
        (long)sizeof(*frame)
    );
    if (written != (long)sizeof(*frame)) {
        fail(role, stage, written < 0 ? written : -EINVAL);
    }
}

static void read_frame(
    int channel,
    struct ivc_frame *frame,
    u64 length,
    unsigned int seed,
    const char *role,
    const char *stage
) {
    unsigned char short_buffer[MESSAGE_SIZE - 1];
    long short_read = syscall3(
        SYS_READ,
        channel,
        (long)short_buffer,
        (long)sizeof(short_buffer)
    );
    if (short_read != -EINVAL) {
        fail(role, stage, short_read < 0 ? short_read : -EINVAL);
    }
    long received = syscall3(
        SYS_READ,
        channel,
        (long)frame,
        (long)sizeof(*frame)
    );
    if (received != (long)sizeof(*frame)) {
        fail(role, stage, received < 0 ? received : -EINVAL);
    }
    memory_barrier();
    if (!frame_matches(frame, length, seed)) {
        fail(role, stage, -EINVAL);
    }
}

static void run_publisher(void) {
    static const char role[] = "publisher";
    unsigned char *local_payload;
    unsigned char *peer_payload;
    struct ivc_frame frame;
    struct ivc_frame ack;
    int channel = open_auto_channel(role);

    expect_second_open_busy(role);
    test_payload_contract(
        channel,
        (void **)&local_payload,
        (void **)&peer_payload,
        role
    );
    fill_payload(local_payload, PUBLISHER_SEED);
    u64 checksum = payload_checksum(local_payload);
    fill_frame(&frame, BULK_SIZE, checksum, 0x31);

    expect_empty_read(channel, role, "before-peer-read");
    long before_peer_write =
        syscall3(SYS_WRITE, channel, (long)&frame, (long)sizeof(frame));
    if (before_peer_write != -EAGAIN) {
        fail(
            role,
            "before-peer-write",
            before_peer_write < 0 ? before_peer_write : -EINVAL
        );
    }
    write_all(
        "AXIVC_STARRY publisher phase=before-peer cache=ENOTTY payload=10MiB local=rw peer=ro protocol=rejected peer-write=rejected peer-mprotect=rejected exec=rejected bad-offset=rejected oversize=rejected write=EAGAIN empty=EAGAIN\n"
    );

    wait_for_events(
        channel,
        POLLOUT,
        POLLOUT,
        POLLERR | POLLHUP | POLLNVAL,
        PEER_WAIT_TIMEOUT_SECONDS,
        role,
        "wait-peer-progress"
    );
    write_frame(channel, &frame, role, "write-bulk-frame");

    wait_for_events(
        channel,
        POLLIN,
        POLLIN,
        POLLERR | POLLHUP | POLLNVAL,
        WAIT_TIMEOUT_SECONDS,
        role,
        "wait-bulk-ack"
    );
    read_frame(
        channel,
        &ack,
        BULK_SIZE,
        0xa5,
        role,
        "read-bulk-ack"
    );
    u64 reverse_checksum = payload_checksum(peer_payload);
    if (ack.checksum != reverse_checksum) {
        fail(role, "verify-bulk-ack", -EINVAL);
    }
    expect_empty_read(channel, role, "empty-read");

    require_zero(syscall3(SYS_CLOSE, channel, 0, 0), role, "close-channel");
    expect_second_open_busy(role);
    unmap_payload(peer_payload, BULK_SIZE, role, "unmap-peer-payload");
    unmap_payload(local_payload, BULK_SIZE, role, "unmap-local-payload");

    write_all(
        "AXIVC_STARRY publisher PASS rx=48 tx=48 bulk=10485760 forward=ok reverse=ok empty=EAGAIN cache=ENOTTY close=ok\n"
    );
    write_all("AXIVC_STARRY publisher CONTROL_DETACH\n");
}

static void run_forced_publisher_wait(void) {
    static const char role[] = "publisher";
    int channel = open_auto_channel(role);
    struct ivc_frame request;
    struct ivc_frame ack;
    unsigned char after_hup;

    wait_for_events(
        channel,
        POLLOUT,
        POLLOUT,
        POLLERR | POLLHUP | POLLNVAL,
        PEER_WAIT_TIMEOUT_SECONDS,
        role,
        "wait-forced-ready"
    );
    fill_frame(
        &request,
        MESSAGE_SIZE,
        FORCED_REQUEST_CHECKSUM,
        FORCED_REQUEST_SEED
    );
    write_frame(channel, &request, role, "write-forced-request");

    wait_for_events(
        channel,
        POLLIN,
        POLLIN,
        POLLERR | POLLHUP | POLLNVAL,
        WAIT_TIMEOUT_SECONDS,
        role,
        "wait-forced-ack"
    );
    read_frame(
        channel,
        &ack,
        MESSAGE_SIZE,
        FORCED_ACK_SEED,
        role,
        "read-forced-ack"
    );
    if (ack.checksum != FORCED_ACK_CHECKSUM) {
        fail(role, "verify-forced-ack", -EINVAL);
    }
    expect_empty_read(channel, role, "forced-empty-read");

    write_all("AXIVC_STARRY publisher phase=forced-wait cause=host-stop\n");
    wait_for_terminal(channel, POLLIN, role, "wait-forced-peer-stop");
    long old_fd_read = syscall3(SYS_READ, channel, (long)&after_hup, 1);
    if (old_fd_read != -EIO) {
        fail(role, "read-after-forced-stop", old_fd_read);
    }
    require_zero(syscall3(SYS_CLOSE, channel, 0, 0), role, "close-forced-channel");
    write_all(
        "AXIVC_STARRY publisher PASS forced=host-stop hup=observed oldfd=EIO close=ok\n"
    );
    write_all("AXIVC_STARRY publisher FORCED_DETACH\n");
}

__attribute__((noreturn)) static void run_subscriber(void) {
    static const char role[] = "subscriber";
    unsigned char *local_payload;
    unsigned char *peer_payload;
    struct ivc_frame frame;
    struct ivc_frame ack;
    int channel = open_auto_channel(role);

    expect_second_open_busy(role);
    test_payload_contract(
        channel,
        (void **)&local_payload,
        (void **)&peer_payload,
        role
    );
    write_all(
        "AXIVC_STARRY subscriber phase=payload-ready cache=ENOTTY payload=10MiB local=rw peer=ro protocol=rejected peer-write=rejected peer-mprotect=rejected exec=rejected bad-offset=rejected oversize=rejected\n"
    );

    wait_for_events(
        channel,
        POLLIN,
        POLLIN,
        POLLERR | POLLHUP | POLLNVAL,
        WAIT_TIMEOUT_SECONDS,
        role,
        "wait-bulk-frame"
    );
    read_frame(
        channel,
        &frame,
        BULK_SIZE,
        0x31,
        role,
        "read-bulk-frame"
    );
    u64 peer_checksum = payload_checksum(peer_payload);
    if (frame.checksum != peer_checksum) {
        fail(role, "verify-bulk-frame", -EINVAL);
    }
    expect_empty_read(channel, role, "empty-read");

    fill_payload(local_payload, SUBSCRIBER_SEED);
    u64 reverse_checksum = payload_checksum(local_payload);
    fill_frame(&ack, BULK_SIZE, reverse_checksum, 0xa5);
    write_frame(channel, &ack, role, "write-bulk-ack");

    wait_for_events(
        channel,
        POLLIN,
        POLLHUP,
        POLLNVAL,
        WAIT_TIMEOUT_SECONDS,
        role,
        "wait-peer-hup"
    );
    unsigned char after_hup;
    long old_fd_read = syscall3(SYS_READ, channel, (long)&after_hup, 1);
    if (old_fd_read != -EIO) {
        fail(role, "read-after-hup", old_fd_read);
    }
    require_zero(syscall3(SYS_CLOSE, channel, 0, 0), role, "close-channel");
    expect_second_open_busy(role);
    unmap_payload(peer_payload, BULK_SIZE, role, "unmap-peer-payload");
    unmap_payload(local_payload, BULK_SIZE, role, "unmap-local-payload");

    write_all(
        "AXIVC_STARRY subscriber PASS rx=48 tx=48 bulk=10485760 forward=ok reverse=ok empty=EAGAIN short=not-consumed hup=observed oldfd=EIO cache=ENOTTY close=ok\n"
    );
    write_all("AXIVC_STARRY subscriber CONTROL_DETACH\n");

    int forced_channel = open_auto_channel(role);
    struct ivc_frame forced_request;
    struct ivc_frame forced_ack;
    wait_for_events(
        forced_channel,
        POLLOUT,
        POLLOUT,
        POLLERR | POLLHUP | POLLNVAL,
        PEER_WAIT_TIMEOUT_SECONDS,
        role,
        "wait-forced-ready"
    );
    wait_for_events(
        forced_channel,
        POLLIN,
        POLLIN,
        POLLERR | POLLHUP | POLLNVAL,
        WAIT_TIMEOUT_SECONDS,
        role,
        "wait-forced-request"
    );
    read_frame(
        forced_channel,
        &forced_request,
        MESSAGE_SIZE,
        FORCED_REQUEST_SEED,
        role,
        "read-forced-request"
    );
    if (forced_request.checksum != FORCED_REQUEST_CHECKSUM) {
        fail(role, "verify-forced-request", -EINVAL);
    }
    fill_frame(
        &forced_ack,
        MESSAGE_SIZE,
        FORCED_ACK_CHECKSUM,
        FORCED_ACK_SEED
    );
    write_frame(
        forced_channel,
        &forced_ack,
        role,
        "write-forced-ack"
    );
    write_all("AXIVC_STARRY subscriber phase=forced-victim-ready cause=host-stop\n");
    park_pid1();
}

void init_main(unsigned long *stack) {
    long argc = (long)stack[0];
    char **argv = (char **)&stack[1];

    open_console();

    if (argc == 2 && same_text(argv[1], "publisher")) {
        run_publisher();
        run_forced_publisher_wait();
        park_pid1();
    } else if (argc == 2 && same_text(argv[1], "subscriber")) {
        run_subscriber();
    } else {
        fail("unknown", "role", -EINVAL);
    }
}
