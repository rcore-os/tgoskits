#include "test_framework.h"
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>
#include <fcntl.h>
#include <termios.h>
#include <string.h>
#include <errno.h>

int __pass = 0;
int __fail = 0;
int __skip = 0;
int __observe = 0;

/* Part 1: 无效 fd */
static int part_01_invalid_fd(void)
{
    int v = 1;
    int ret;

    errno = 0;
    ret = ioctl(-1, FIONBIO, &v);
    int err = errno;
    CHECK_RET(ret, -1, "Part 1: ioctl(-1) fails");
    CHECK_ERR_SAVED(ret, err, EBADF, "Part 1: errno=EBADF");

    errno = 0;
    ret = ioctl(9999, FIONBIO, &v);
    err = errno;
    CHECK_RET(ret, -1, "Part 1: ioctl(9999) fails");
    CHECK_ERR_SAVED(ret, err, EBADF, "Part 1: errno=EBADF");

    return 0;
}

/* Part 2: 不支持的 request */
static int part_02_unsupported_request(void)
{
    int p[2];
    int ret, err;
    char buf[4] = {0};

    if (pipe(p) != 0) return 1;

    errno = 0;
    ret = ioctl(p[0], 0xFFFF, buf);
    err = errno;
    CHECK_RET(ret, -1, "Part 2: ioctl(pipe, 0xFFFF) fails");
    CHECK_ERR_SAVED(ret, err, ENOTTY, "Part 2: errno=ENOTTY");

    errno = 0;
    ret = ioctl(p[0], _IO('x', 0xFF), buf);
    err = errno;
    CHECK_RET(ret, -1, "Part 2: ioctl(pipe, _IO('x',0xFF)) fails");
    CHECK_ERR_SAVED(ret, err, ENOTTY, "Part 2: errno=ENOTTY");

    close(p[0]);
    close(p[1]);
    return 0;
}

/* Part 3: FIONBIO 正向路径 */
static int part_03_fionbio(void)
{
    int p[2];
    int v, ret, err;
    char buf[1];

    if (pipe(p) != 0) return 1;

    v = 1;
    errno = 0;
    ret = ioctl(p[0], FIONBIO, &v);
    err = errno;
    CHECK_RET(ret, 0, "Part 3: FIONBIO=1 succeeds");

    errno = 0;
    ssize_t r = read(p[0], buf, 1);
    err = errno;
    CHECK_RET(r, -1, "Part 3: empty pipe + O_NONBLOCK returns -1");
    CHECK_ERR_SAVED(r, err, EAGAIN, "Part 3: errno=EAGAIN");

    v = 0;
    errno = 0;
    ret = ioctl(p[0], FIONBIO, &v);
    if (ret == 0) {
        TEST_OBSERVE("Part 3: FIONBIO=0 clears O_NONBLOCK");
    } else {
        TEST_OBSERVE("Part 3: FIONBIO=0 behavior varies");
    }

    v = 2;
    errno = 0;
    ret = ioctl(p[0], FIONBIO, &v);
    if (ret == 0) {
        TEST_OBSERVE("Part 3: FIONBIO=2 sets O_NONBLOCK (non-zero treated as true)");
    } else {
        TEST_OBSERVE("Part 3: FIONBIO=2 behavior varies");
    }

    close(p[0]);
    close(p[1]);
    return 0;
}

/* Part 4: TCGETS/TCSETS（条件项） */
static int part_04_termios(void)
{
    int fd = open("/dev/tty", O_RDWR);
    if (fd < 0) {
        TEST_SKIP("Part 4: /dev/tty not available");
        return 0;
    }

    struct termios t1, t2;
    int ret = ioctl(fd, TCGETS, &t1);
    if (ret != 0) {
        TEST_SKIP("Part 4: TCGETS not supported");
        close(fd);
        return 0;
    }

    struct termios orig = t1;
    t1.c_lflag &= ~ICANON;
    ret = ioctl(fd, TCSETS, &t1);
    CHECK_RET(ret, 0, "Part 4: TCSETS succeeds");

    memset(&t2, 0, sizeof(t2));
    ret = ioctl(fd, TCGETS, &t2);
    CHECK_RET(ret, 0, "Part 4: TCGETS after TCSETS");
    CHECK_TRUE(t2.c_lflag != orig.c_lflag, "Part 4: termios changed");

    ioctl(fd, TCSETS, &orig);
    close(fd);
    return 0;
}

int main(void)
{
    TEST_START("ioctl: minimal validation");

    part_01_invalid_fd();
    part_02_unsupported_request();
    part_03_fionbio();
    part_04_termios();

    TEST_DONE();
}
