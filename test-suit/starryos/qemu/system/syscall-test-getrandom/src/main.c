#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <sys/wait.h>
#include <termios.h>
#include <unistd.h>

#ifndef GRND_INSECURE
#define GRND_INSECURE 0x0004
#endif

#define RNDGETENTCNT 0x80045200
#define RNDADDTOENTCNT 0x40045201
#define RNDADDENTROPY 0x40085203
#define RNDZAPENTCNT 0x5204
#define RNDCLEARPOOL 0x5206
#define RNDRESEEDCRNG 0x5207

struct pool_info {
    int entropy_count;
    int buf_size;
    unsigned char buf[8];
};

/* random_ioctl(), which both devices share. */
static void check_ioctl(const char *path)
{
    int fd = open(path, O_RDWR);
    CHECK(fd >= 0, "open random device for ioctl");
    if (fd < 0) {
        return;
    }

    int bits = -1;
    CHECK_RET(ioctl(fd, RNDGETENTCNT, &bits), 0, "RNDGETENTCNT succeeds");
    CHECK(bits == 256, "a seeded pool reports 256 bits");
    CHECK_ERR(ioctl(fd, RNDGETENTCNT, NULL), EFAULT, "RNDGETENTCNT faults on a bad pointer");

    int credit = 8;
    CHECK_RET(ioctl(fd, RNDADDTOENTCNT, &credit), 0, "CAP_SYS_ADMIN may credit entropy");
    credit = -1;
    CHECK_ERR(ioctl(fd, RNDADDTOENTCNT, &credit), EINVAL, "a negative credit is rejected");

    struct pool_info pool = {.entropy_count = 8, .buf_size = 8, .buf = "entropy"};
    CHECK_RET(ioctl(fd, RNDADDENTROPY, &pool), 0, "CAP_SYS_ADMIN may add entropy");
    pool.entropy_count = -1;
    CHECK_ERR(ioctl(fd, RNDADDENTROPY, &pool), EINVAL, "a negative entropy count is rejected");
    pool.entropy_count = 8;

    long page = sysconf(_SC_PAGESIZE);
    unsigned char *map =
        mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(map != MAP_FAILED, "mmap two pages");
    if (map != MAP_FAILED) {
        CHECK_RET(mprotect(map + page, page, PROT_NONE), 0, "protect the second page");
        int *header = (int *)(map + page - 2 * sizeof(int));
        header[0] = 8;
        header[1] = 64;
        CHECK_ERR(ioctl(fd, RNDADDENTROPY, header), EFAULT,
                  "entropy that cannot be copied whole faults");
        munmap(map, 2 * page);
    }

    CHECK_RET(ioctl(fd, RNDZAPENTCNT), 0, "RNDZAPENTCNT is accepted");
    CHECK_RET(ioctl(fd, RNDCLEARPOOL), 0, "RNDCLEARPOOL is accepted");
    CHECK_RET(ioctl(fd, RNDRESEEDCRNG), 0, "RNDRESEEDCRNG reseeds a ready CRNG");
    CHECK_ERR(ioctl(fd, TCGETS, &pool), EINVAL, "other commands are EINVAL");

    pid_t pid = fork();
    if (pid == 0) {
        int ok = setgid(65534) == 0 && setuid(65534) == 0;
        credit = 8;
        ok = ok && ioctl(fd, RNDGETENTCNT, &bits) == 0;
        ok = ok && ioctl(fd, RNDADDTOENTCNT, &credit) == -1 && errno == EPERM;
        ok = ok && ioctl(fd, RNDADDENTROPY, &pool) == -1 && errno == EPERM;
        ok = ok && ioctl(fd, RNDZAPENTCNT) == -1 && errno == EPERM;
        ok = ok && ioctl(fd, RNDCLEARPOOL) == -1 && errno == EPERM;
        ok = ok && ioctl(fd, RNDRESEEDCRNG) == -1 && errno == EPERM;
        _exit(ok ? 0 : 1);
    }
    int status = -1;
    CHECK(pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status)
              && WEXITSTATUS(status) == 0,
          "without CAP_SYS_ADMIN only RNDGETENTCNT is allowed");
    close(fd);
}

static void check_device(const char *path, unsigned int dev_minor, short revents)
{
    struct stat st;
    unsigned char a[32];
    unsigned char b[32];

    CHECK_RET(stat(path, &st), 0, path);
    CHECK(S_ISCHR(st.st_mode) && major(st.st_rdev) == 1 && minor(st.st_rdev) == dev_minor,
          "random device number matches Linux");

    int fd = open(path, O_RDWR | O_NONBLOCK);
    CHECK(fd >= 0, "open random device O_RDWR|O_NONBLOCK");
    if (fd < 0) {
        return;
    }
    /* A seeded CRNG never makes a nonblocking read return EAGAIN. */
    CHECK_RET(read(fd, a, sizeof(a)), sizeof(a), "nonblocking read returns the full request");
    CHECK_RET(read(fd, b, sizeof(b)), sizeof(b), "second nonblocking read returns the full request");
    CHECK(memcmp(a, b, sizeof(a)) != 0, "consecutive reads differ");
    CHECK_RET(write(fd, "seed", 4), 4, "writes are accepted");

    struct pollfd pfd = {.fd = fd, .events = POLLIN | POLLOUT};
    CHECK_RET(poll(&pfd, 1, 0), 1, "poll reports readiness");
    CHECK(pfd.revents == revents, "poll revents match Linux");
    close(fd);
}

int main(void)
{
    unsigned char a[64];
    unsigned char b[64];

    TEST_START("getrandom and random devices");

    /* Linux random_read_iter() waits through wait_for_random_bytes(), which
     * gathers entropy itself, so a blocking read returns even as the first
     * consumer on a platform that boots unseeded. */
    printf("  CRNG %s before the first blocking read\n",
           getrandom(a, 1, GRND_NONBLOCK) == 1 ? "ready" : "not ready");
    int rnd = open("/dev/random", O_RDONLY);
    CHECK(rnd >= 0, "open /dev/random for blocking reads");
    if (rnd >= 0) {
        CHECK_RET(read(rnd, a, 16), 16, "a blocking /dev/random read returns the full request");
        CHECK_RET(pread(rnd, a, 16, 0), 16, "pread on /dev/random returns the full request");
        close(rnd);
    }

    /* A blocking call returns once the CRNG is seeded, whether by firmware,
     * the CPU or timing jitter; nonblocking calls never see EAGAIN after it. */
    CHECK_RET(getrandom(b, sizeof(b), 0), sizeof(b), "blocking getrandom returns the full request");
    CHECK_RET(getrandom(a, sizeof(a), GRND_NONBLOCK), sizeof(a),
              "GRND_NONBLOCK does not return EAGAIN once seeded");
    CHECK(memcmp(a, b, sizeof(a)) != 0, "consecutive getrandom outputs differ");
    CHECK_RET(getrandom(a, 16, GRND_RANDOM | GRND_NONBLOCK), 16,
              "GRND_RANDOM does not return EAGAIN once seeded");
    CHECK_RET(getrandom(a, 16, GRND_INSECURE), 16, "GRND_INSECURE returns bytes");

    CHECK_ERR(getrandom(a, 16, 0x80), EINVAL, "unknown flags are rejected");
    CHECK_ERR(getrandom(a, 16, GRND_INSECURE | GRND_RANDOM), EINVAL,
              "GRND_INSECURE|GRND_RANDOM is rejected");
    CHECK_ERR(getrandom(a, 0, 0x80), EINVAL, "flags are checked before the length");
    CHECK_RET(syscall(SYS_getrandom, NULL, 0, 0), 0, "zero length returns zero");
    CHECK_ERR(syscall(SYS_getrandom, NULL, 16, 0), EFAULT, "an unmapped buffer faults");

    long page = sysconf(_SC_PAGESIZE);
    unsigned char *map =
        mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(map != MAP_FAILED, "mmap two pages");
    if (map != MAP_FAILED) {
        CHECK_RET(mprotect(map + page, page, PROT_NONE), 0, "protect the second page");
        /* Linux returns the bytes copied before the faulting page. */
        CHECK_RET(getrandom(map + page - 100, 200, 0), 100, "getrandom stops at the faulting page");
        munmap(map, 2 * page);
    }

    /* /dev/random polls readable once seeded; /dev/urandom has no poll hook. */
    check_device("/dev/random", 8, POLLIN);
    check_device("/dev/urandom", 9, POLLIN | POLLOUT);
    check_ioctl("/dev/random");
    check_ioctl("/dev/urandom");

    TEST_DONE();
}
