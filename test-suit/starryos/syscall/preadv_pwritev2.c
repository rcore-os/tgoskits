#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef EOPNOTSUPP
#define EOPNOTSUPP 95
#endif

static int failures;

static long raw_preadv(int fd, const struct iovec *iov, unsigned long iovcnt, long offset)
{
    errno = 0;
    return syscall(SYS_preadv, fd, iov, iovcnt, offset, 0L);
}

static long raw_pwritev(int fd, const struct iovec *iov, unsigned long iovcnt, long offset)
{
    errno = 0;
    return syscall(SYS_pwritev, fd, iov, iovcnt, offset, 0L);
}

static long raw_preadv2(int fd, const struct iovec *iov, unsigned long iovcnt, long offset,
                        int flags)
{
    errno = 0;
    return syscall(SYS_preadv2, fd, iov, iovcnt, offset, 0L, flags);
}

static long raw_pwritev2(int fd, const struct iovec *iov, unsigned long iovcnt, long offset,
                         int flags)
{
    errno = 0;
    return syscall(SYS_pwritev2, fd, iov, iovcnt, offset, 0L, flags);
}

static void check_num(const char *name, long got, long want)
{
    if (got == want) {
        printf("PASS %s=%ld\n", name, got);
    } else {
        printf("FAIL %s got=%ld want=%ld\n", name, got, want);
        failures++;
    }
}

static void check_str(const char *name, const char *got, const char *want)
{
    if (strcmp(got, want) == 0) {
        printf("PASS %s='%s'\n", name, got);
    } else {
        printf("FAIL %s got='%s' want='%s'\n", name, got, want);
        failures++;
    }
}

static void check_ret_errno(const char *name, long ret, long want_ret, int got_errno,
                            int want_errno)
{
    if (ret == want_ret && got_errno == want_errno) {
        printf("PASS %s ret=%ld errno=%d\n", name, ret, got_errno);
    } else {
        printf("FAIL %s ret=%ld errno=%d want_ret=%ld want_errno=%d\n", name, ret,
               got_errno, want_ret, want_errno);
        failures++;
    }
}

static int reset_file(int fd, const char *content)
{
    if (ftruncate(fd, 0) != 0) {
        perror("ftruncate");
        return -1;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) {
        perror("lseek reset");
        return -1;
    }
    size_t len = strlen(content);
    if (write(fd, content, len) != (ssize_t)len) {
        perror("write reset");
        return -1;
    }
    return 0;
}

static int read_content(int fd, char *buf, size_t size)
{
    off_t cur = lseek(fd, 0, SEEK_CUR);
    if (cur < 0) {
        perror("lseek cur");
        return -1;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) {
        perror("lseek start");
        return -1;
    }
    ssize_t n = read(fd, buf, size - 1);
    if (n < 0) {
        perror("read content");
        return -1;
    }
    buf[n] = '\0';
    if (lseek(fd, cur, SEEK_SET) < 0) {
        perror("lseek restore");
        return -1;
    }
    return 0;
}

static void basic_offset_cases(int fd)
{
    char a[] = "AA";
    char b[] = "B";
    struct iovec wiov[2] = {
        { .iov_base = a, .iov_len = 2 },
        { .iov_base = b, .iov_len = 1 },
    };
    char buf[64];
    char r1[4] = {0};
    char r2[3] = {0};
    struct iovec riov[2] = {
        { .iov_base = r1, .iov_len = 3 },
        { .iov_base = r2, .iov_len = 2 },
    };

    reset_file(fd, "0123456789");
    lseek(fd, 3, SEEK_SET);
    long ret = raw_pwritev(fd, wiov, 2, 5);
    check_ret_errno("pwritev_offset_ret", ret, 3, ret < 0 ? errno : 0, 0);
    read_content(fd, buf, sizeof(buf));
    check_str("pwritev_offset_content", buf, "01234AAB89");
    check_num("pwritev_offset_cur", lseek(fd, 0, SEEK_CUR), 3);

    lseek(fd, 4, SEEK_SET);
    ret = raw_preadv(fd, riov, 2, 0);
    check_ret_errno("preadv_offset_ret", ret, 5, ret < 0 ? errno : 0, 0);
    check_str("preadv_offset_buf", r1, "012");
    check_str("preadv_offset_buf2", r2, "34");
    check_num("preadv_offset_cur", lseek(fd, 0, SEEK_CUR), 4);

    reset_file(fd, "0123456789");
    lseek(fd, 3, SEEK_SET);
    ret = raw_pwritev2(fd, wiov, 2, 5, 0);
    check_ret_errno("pwritev2_offset_ret", ret, 3, ret < 0 ? errno : 0, 0);
    read_content(fd, buf, sizeof(buf));
    check_str("pwritev2_offset_content", buf, "01234AAB89");
    check_num("pwritev2_offset_cur", lseek(fd, 0, SEEK_CUR), 3);
}

static void offset_minus_one_cases(int fd)
{
    char a[] = "AA";
    char b[] = "B";
    struct iovec wiov[2] = {
        { .iov_base = a, .iov_len = 2 },
        { .iov_base = b, .iov_len = 1 },
    };
    char buf[64];
    char r1[4] = {0};
    char r2[3] = {0};
    struct iovec riov[2] = {
        { .iov_base = r1, .iov_len = 3 },
        { .iov_base = r2, .iov_len = 2 },
    };

    reset_file(fd, "0123456789");
    lseek(fd, 2, SEEK_SET);
    long ret = raw_pwritev2(fd, wiov, 2, -1, 0);
    check_ret_errno("pwritev2_minus1_ret", ret, 3, ret < 0 ? errno : 0, 0);
    read_content(fd, buf, sizeof(buf));
    check_str("pwritev2_minus1_content", buf, "01AAB56789");
    check_num("pwritev2_minus1_cur", lseek(fd, 0, SEEK_CUR), 5);

    lseek(fd, 1, SEEK_SET);
    ret = raw_preadv2(fd, riov, 2, -1, 0);
    check_ret_errno("preadv2_minus1_ret", ret, 5, ret < 0 ? errno : 0, 0);
    check_str("preadv2_minus1_buf", r1, "1AA");
    check_str("preadv2_minus1_buf2", r2, "B5");
    check_num("preadv2_minus1_cur", lseek(fd, 0, SEEK_CUR), 6);
}

static void boundary_cases(int fd)
{
    char x[] = "X";
    char y[] = "Y";
    char buf[64];
    struct iovec zero_first[2] = {
        { .iov_base = x, .iov_len = 0 },
        { .iov_base = y, .iov_len = 1 },
    };
    struct iovec huge[2] = {
        { .iov_base = x, .iov_len = SSIZE_MAX },
        { .iov_base = y, .iov_len = 1 },
    };
    struct iovec bad_base = { .iov_base = (void *)1, .iov_len = 1 };

    reset_file(fd, "0123456789");
    long ret = raw_pwritev(fd, NULL, 0, 0);
    check_ret_errno("pwritev_iovcnt0", ret, 0, ret < 0 ? errno : 0, 0);

    ret = raw_pwritev(fd, zero_first, 2, 0);
    check_ret_errno("pwritev_zero_len", ret, 1, ret < 0 ? errno : 0, 0);
    read_content(fd, buf, sizeof(buf));
    check_str("pwritev_zero_len_content", buf, "Y123456789");

    ret = raw_pwritev(fd, zero_first, 1025, 0);
    check_ret_errno("pwritev_iovcnt_large", ret, -1, ret < 0 ? errno : 0, EINVAL);

    ret = raw_pwritev(fd, huge, 2, 0);
    check_ret_errno("pwritev_len_overflow", ret, -1, ret < 0 ? errno : 0, EFAULT);

    ret = raw_pwritev(fd, (const struct iovec *)1, 1, 0);
    check_ret_errno("pwritev_bad_iov_ptr", ret, -1, ret < 0 ? errno : 0, EFAULT);

    ret = raw_pwritev(fd, &bad_base, 1, 0);
    check_ret_errno("pwritev_bad_iov_base", ret, -1, ret < 0 ? errno : 0, EFAULT);
}

static void fd_and_flag_cases(void)
{
    char x[] = "X";
    char rbuf[2] = {0};
    struct iovec wiov = { .iov_base = x, .iov_len = 1 };
    struct iovec riov = { .iov_base = rbuf, .iov_len = 1 };

    long ret = raw_pwritev(-1, &wiov, 1, 0);
    check_ret_errno("pwritev_bad_fd", ret, -1, ret < 0 ? errno : 0, EBADF);

    int ro = open("/tmp/pv_ro", O_CREAT | O_TRUNC | O_RDONLY, 0600);
    ret = raw_pwritev(ro, &wiov, 1, 0);
    check_ret_errno("pwritev_readonly_fd", ret, -1, ret < 0 ? errno : 0, EBADF);
    close(ro);

    int wo = open("/tmp/pv_wo", O_CREAT | O_TRUNC | O_WRONLY, 0600);
    ret = raw_preadv(wo, &riov, 1, 0);
    check_ret_errno("preadv_writeonly_fd", ret, -1, ret < 0 ? errno : 0, EBADF);
    close(wo);

    int fd = open("/tmp/pv_flags", O_CREAT | O_TRUNC | O_RDWR, 0600);
    ret = raw_pwritev2(fd, &wiov, 1, 0, 0x40000000);
    check_ret_errno("pwritev2_unknown_flags", ret, -1, ret < 0 ? errno : 0, EOPNOTSUPP);
    ret = raw_preadv2(fd, &riov, 1, 0, 0x40000000);
    check_ret_errno("preadv2_unknown_flags", ret, -1, ret < 0 ? errno : 0, EOPNOTSUPP);
    close(fd);
}

static void append_cases(void)
{
    char a[] = "AA";
    struct iovec wiov = { .iov_base = a, .iov_len = 2 };
    char buf[64];

    int fd = open("/tmp/pv_append", O_CREAT | O_TRUNC | O_RDWR | O_APPEND, 0600);
    if (fd < 0) {
        perror("open append");
        failures++;
        return;
    }
    if (write(fd, "abc", 3) != 3) {
        perror("write append seed");
        failures++;
    }
    lseek(fd, 0, SEEK_SET);
    long ret = raw_pwritev(fd, &wiov, 1, 0);
    check_ret_errno("append_pwritev_ret", ret, 2, ret < 0 ? errno : 0, 0);
    read_content(fd, buf, sizeof(buf));
    check_str("append_pwritev_content", buf, "abcAA");
    check_num("append_pwritev_cur", lseek(fd, 0, SEEK_CUR), 0);

    lseek(fd, 0, SEEK_SET);
    ret = raw_pwritev2(fd, &wiov, 1, 0, 0);
    check_ret_errno("append_pwritev2_ret", ret, 2, ret < 0 ? errno : 0, 0);
    read_content(fd, buf, sizeof(buf));
    check_str("append_pwritev2_content", buf, "abcAAAA");
    check_num("append_pwritev2_cur", lseek(fd, 0, SEEK_CUR), 0);
    close(fd);
}

int main(void)
{
    int fd = open("/tmp/pv_main", O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0) {
        perror("open main");
        return 1;
    }

    basic_offset_cases(fd);
    offset_minus_one_cases(fd);
    boundary_cases(fd);
    close(fd);
    fd_and_flag_cases();
    append_cases();

    if (failures == 0) {
        printf("PV_TEST_PASS\n");
        return 0;
    }
    printf("PV_TEST_FAIL failures=%d\n", failures);
    return 1;
}
