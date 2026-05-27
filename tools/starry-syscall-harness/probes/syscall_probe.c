#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef SYS_memfd_create
#if defined(__x86_64__)
#define SYS_memfd_create 319
#elif defined(__riscv)
#define SYS_memfd_create 279
#elif defined(__aarch64__)
#define SYS_memfd_create 279
#elif defined(__loongarch__)
#define SYS_memfd_create 279
#endif
#endif

#ifndef SYS_pwritev2
#if defined(__x86_64__)
#define SYS_pwritev2 328
#elif defined(__riscv)
#define SYS_pwritev2 287
#elif defined(__aarch64__)
#define SYS_pwritev2 287
#elif defined(__loongarch__)
#define SYS_pwritev2 287
#endif
#endif

static int saved_errno(long ret)
{
    return ret < 0 ? errno : 0;
}

static void case_pipe2_invalid_flags(void)
{
    int fds[2] = {-1, -1};
    errno = 0;
    long ret = syscall(SYS_pipe2, fds, 0x80000000U);
    int err = saved_errno(ret);
    int created = ret == 0;
    if (created) {
        close(fds[0]);
        close(fds[1]);
    }

    printf("CASE pipe2_invalid_flags ret=%ld errno=%d created=%d\n", ret, err, created);
}

static void case_eventfd2_invalid_flags(void)
{
    errno = 0;
    long ret = syscall(SYS_eventfd2, 0, 0x80000000U);
    int err = saved_errno(ret);
    int created = ret >= 0;
    if (created) {
        close((int)ret);
    }

    printf("CASE eventfd2_invalid_flags ret=%ld errno=%d created=%d\n", ret, err, created);
}

static void case_memfd_create_invalid_flags(void)
{
#ifdef SYS_memfd_create
    errno = 0;
    long ret = syscall(SYS_memfd_create, "starry-probe", 0x80000000U);
    int err = saved_errno(ret);
    int created = ret >= 0;
    if (created) {
        close((int)ret);
    }

    printf("CASE memfd_create_invalid_flags ret=%ld errno=%d created=%d\n", ret, err, created);
#else
    printf("CASE memfd_create_invalid_flags skipped=1\n");
#endif
}

static void case_dup3_same_fd(void)
{
    int fd = open("/dev/null", O_RDONLY);
    if (fd < 0) {
        printf("CASE dup3_same_fd setup_errno=%d\n", errno);
        return;
    }

    errno = 0;
    long ret = syscall(SYS_dup3, fd, fd, O_CLOEXEC);
    int err = saved_errno(ret);
    close(fd);

    printf("CASE dup3_same_fd ret=%ld errno=%d\n", ret, err);
}

static void case_pwritev2_writes_data(void)
{
#ifdef SYS_pwritev2
    char path[96];
    snprintf(path, sizeof(path), "/tmp/starry-pwritev2-%ld", (long)getpid());
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0600);
    if (fd < 0) {
        printf("CASE pwritev2_writes_data setup_errno=%d\n", errno);
        return;
    }

    const char data[] = "XY";
    struct iovec iov = {
        .iov_base = (void *)data,
        .iov_len = 2,
    };
    errno = 0;
    long ret = syscall(SYS_pwritev2, fd, &iov, 1, 0, 0, 0);
    int err = saved_errno(ret);

    char read_buf[3] = {0, 0, 0};
    lseek(fd, 0, SEEK_SET);
    ssize_t read_ret = read(fd, read_buf, 2);
    int read_err = read_ret < 0 ? errno : 0;
    close(fd);
    unlink(path);

    printf(
        "CASE pwritev2_writes_data ret=%ld errno=%d read_ret=%ld read_errno=%d data=%02x%02x\n",
        ret,
        err,
        (long)read_ret,
        read_err,
        (unsigned char)read_buf[0],
        (unsigned char)read_buf[1]);
#else
    printf("CASE pwritev2_writes_data skipped=1\n");
#endif
}

static void case_ftruncate_readonly_fd(void)
{
    char path[96];
    snprintf(path, sizeof(path), "/tmp/starry-ftruncate-%ld", (long)getpid());
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
    if (fd < 0) {
        printf("CASE ftruncate_readonly_fd setup_errno=%d\n", errno);
        return;
    }
    if (write(fd, "abc", 3) != 3) {
        int setup_errno = errno;
        close(fd);
        unlink(path);
        printf("CASE ftruncate_readonly_fd setup_errno=%d\n", setup_errno);
        return;
    }
    close(fd);

    fd = open(path, O_RDONLY);
    if (fd < 0) {
        int setup_errno = errno;
        unlink(path);
        printf("CASE ftruncate_readonly_fd setup_errno=%d\n", setup_errno);
        return;
    }

    errno = 0;
    int ret = ftruncate(fd, 1);
    int err = saved_errno(ret);
    int not_open_for_write = ret == -1 && (err == EBADF || err == EINVAL);
    close(fd);
    unlink(path);

    printf("CASE ftruncate_readonly_fd ret=%d not_open_for_write=%d\n",
           ret,
           not_open_for_write);
}

int main(void)
{
    puts("STARRY_SYSCALL_PROBE_BEGIN");
    case_pipe2_invalid_flags();
    case_eventfd2_invalid_flags();
    case_memfd_create_invalid_flags();
    case_dup3_same_fd();
    case_pwritev2_writes_data();
    case_ftruncate_readonly_fd();
    puts("STARRY_SYSCALL_PROBE_END");
    fflush(stdout);
    return 0;
}
