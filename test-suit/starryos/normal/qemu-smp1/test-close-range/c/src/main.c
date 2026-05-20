#include "test_framework.h"
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include <fcntl.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <string.h>
#include <errno.h>

int __pass = 0;
int __fail = 0;
int __skip = 0;
int __observe = 0;

#ifndef CLOSE_RANGE_CLOEXEC
#define CLOSE_RANGE_CLOEXEC (1U << 2)
#endif

static int do_close_range(unsigned int first, unsigned int last, unsigned int flags)
{
    long ret = syscall(__NR_close_range, first, last, flags);
    return (int)ret;
}

static void safe_close(int *fd)
{
    if (fd && *fd >= 0) {
        close(*fd);
        *fd = -1;
    }
}

#define TMPFILE "/tmp/test_close_range_file"

static int create_temp_file(const char *data)
{
    int fd = open(TMPFILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) return -1;
    if (data) {
        ssize_t n = write(fd, data, strlen(data));
        if (n < 0) { close(fd); return -1; }
    }
    close(fd);
    return 0;
}

/* Part 1: close_range 基本关闭 */
static int part_01_basic_close(void)
{
    int origin, n = 6;
    int fds[6];
    int low_guard, high_guard;
    char buf[1];

    unlink(TMPFILE);
    create_temp_file("close_range_data");

    origin = open(TMPFILE, O_RDWR);
    CHECK_TRUE(origin >= 0, "Part 1: open origin");
    if (origin < 0) return 1;

    low_guard = fcntl(origin, F_DUPFD, 100);
    for (int i = 0; i < n; i++)
        fds[i] = fcntl(origin, F_DUPFD, low_guard + 1 + i);
    high_guard = fcntl(origin, F_DUPFD, fds[n-1] + 1);

    CHECK_TRUE(low_guard < fds[0], "Part 1: low guard below range");
    CHECK_TRUE(high_guard > fds[n-1], "Part 1: high guard above range");

    int ret = do_close_range(fds[0], fds[n-1], 0);
    CHECK_RET(ret, 0, "Part 1: do_close_range succeeds");

    for (int i = 0; i < n; i++) {
        errno = 0;
        ssize_t r = read(fds[i], buf, 1);
        int err = errno;
        CHECK_RET(r, -1, "Part 1: closed fd returns -1");
        CHECK_ERR_SAVED(r, err, EBADF, "Part 1: closed fd returns EBADF");
    }

    errno = 0;
    int gf = fcntl(low_guard, F_GETFD);
    CHECK_TRUE(gf >= 0, "Part 1: low guard still valid");
    errno = 0;
    gf = fcntl(high_guard, F_GETFD);
    CHECK_TRUE(gf >= 0, "Part 1: high guard still valid");

    safe_close(&origin);
    safe_close(&low_guard);
    safe_close(&high_guard);
    unlink(TMPFILE);
    return 0;
}

/* Part 2: 范围精确性 */
static int part_02_range_precision(void)
{
    int origin, n = 6;
    int fds[6];
    int low_guard, high_guard;
    char buf[1];

    unlink(TMPFILE);
    create_temp_file("close_range_data");

    origin = open(TMPFILE, O_RDWR);
    CHECK_TRUE(origin >= 0, "Part 2: open origin");
    if (origin < 0) return 1;

    low_guard = fcntl(origin, F_DUPFD, 100);
    for (int i = 0; i < n; i++)
        fds[i] = fcntl(origin, F_DUPFD, low_guard + 1 + i);
    high_guard = fcntl(origin, F_DUPFD, fds[n-1] + 1);

    CHECK_TRUE(low_guard < fds[0], "Part 2: low guard below range");
    CHECK_TRUE(high_guard > fds[n-1], "Part 2: high guard above range");

    int ret = do_close_range(fds[1], fds[n-2], 0);
    CHECK_RET(ret, 0, "Part 2: do_close_range middle range");

    errno = 0;
    int g1 = fcntl(fds[0], F_GETFD);
    CHECK_TRUE(g1 >= 0, "Part 2: fds[0] still valid");
    errno = 0;
    int g2 = fcntl(fds[n-1], F_GETFD);
    CHECK_TRUE(g2 >= 0, "Part 2: fds[n-1] still valid");

    for (int i = 1; i < n-1; i++) {
        errno = 0;
        ssize_t r = read(fds[i], buf, 1);
        int err = errno;
        CHECK_RET(r, -1, "Part 2: middle fd closed");
        CHECK_ERR_SAVED(r, err, EBADF, "Part 2: middle fd returns EBADF");
    }

    safe_close(&origin);
    safe_close(&low_guard);
    safe_close(&high_guard);
    unlink(TMPFILE);
    return 0;
}

/* Part 3: 不存在 fd 的静默处理 */
static int part_03_empty_fd_range(void)
{
    int empty_start = -1, count = 0;

    for (int fd = 500; fd < 2000; fd++) {
        errno = 0;
        int r = fcntl(fd, F_GETFD);
        if (r < 0 && errno == EBADF) {
            if (empty_start < 0) empty_start = fd;
            count++;
            if (count >= 10) break;
        } else {
            empty_start = -1;
            count = 0;
        }
    }

    if (count < 10) {
        TEST_SKIP("Part 3: cannot find 10 consecutive empty fds in [500, 2000]");
        return 0;
    }

    int ret = do_close_range(empty_start, empty_start + 9, 0);
    CHECK_RET(ret, 0, "Part 3: close_range on empty fds succeeds");

    return 0;
}

/* Part 4: CLOSE_RANGE_CLOEXEC */
static int part_04_cloexec(void)
{
    int origin, n = 6;
    int fds[6];

    unlink(TMPFILE);
    create_temp_file("close_range_data");

    origin = open(TMPFILE, O_RDWR);
    CHECK_TRUE(origin >= 0, "Part 4: open origin");
    if (origin < 0) return 1;

    for (int i = 0; i < n; i++)
        fds[i] = fcntl(origin, F_DUPFD, 100 + i);

    int ret = do_close_range(fds[0], fds[n-1], CLOSE_RANGE_CLOEXEC);
    CHECK_RET(ret, 0, "Part 4: do_close_range CLOEXEC succeeds");

    for (int i = 0; i < n; i++) {
        errno = 0;
        int f = fcntl(fds[i], F_GETFD);
        int err = errno;
        CHECK_TRUE(f >= 0, "Part 4: fd still valid");
        CHECK_TRUE((f & FD_CLOEXEC) != 0, "Part 4: FD_CLOEXEC set");
        (void)err;
    }

    for (int i = 0; i < n; i++)
        safe_close(&fds[i]);
    safe_close(&origin);
    unlink(TMPFILE);
    return 0;
}

/* Part 5: 负向测试 */
static int part_05_negative(void)
{
    unsigned int bad_flags = 1U << 31;
    errno = 0;
    int ret = do_close_range(0, 100, bad_flags);
    int err = errno;
    CHECK_RET(ret, -1, "Part 5: invalid flags fails");
    CHECK_ERR_SAVED(ret, err, EINVAL, "Part 5: errno=EINVAL");

    int ret2 = do_close_range(10, 5, 0);
    (void)ret2;
    TEST_OBSERVE("Part 5: start > end behavior varies by implementation");

    return 0;
}

int main(void)
{
    TEST_START("close_range: semantic validation");

    part_01_basic_close();
    part_02_range_precision();
    part_03_empty_fd_range();
    part_04_cloexec();
    part_05_negative();

    TEST_DONE();
}
