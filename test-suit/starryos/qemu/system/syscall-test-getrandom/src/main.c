#include "test_framework.h"

#include <fcntl.h>
#include <poll.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#ifndef GRND_INSECURE
#define GRND_INSECURE 0x0004
#endif

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

    TEST_DONE();
}
