#include "test_framework.h"
#include "test_helpers.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <string.h>
#include <errno.h>

#ifndef F_DUPFD_CLOEXEC
#define F_DUPFD_CLOEXEC 1030
#endif

int parts_dup_dup3_fcntl(void)
{
    int fail = 0;
    int fd;

    /* PART 1: 基本 dup */

    create_temp_file_with_data(TMPFILE, "hello");

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd >= 0, "openat 创建文件成功");
    if (fd < 0) return 1;

    int fd2 = dup(fd);
    CHECK(fd2 >= 0, "dup 成功");
    CHECK(fd2 != fd, "dup 返回不同的 fd");

    lseek(fd2, 0, SEEK_SET);
    char buf[16] = {0};
    ssize_t n = read(fd2, buf, 5);
    CHECK_RET(n, 5, "通过 dup fd 读回完整数据");
    CHECK(memcmp(buf, "hello", 5) == 0, "dup fd 读回内容正确");

    close(fd2);

    /* PART 2: dup 后关闭原 fd */

    int fd_dup = dup(fd);
    CHECK(fd_dup >= 0, "dup(fd) for close-original test");

    CHECK_RET(close(fd), 0, "关闭原 fd");

    lseek(fd_dup, 0, SEEK_SET);
    char buf2[16] = {0};
    ssize_t n2 = read(fd_dup, buf2, 5);
    CHECK_RET(n2, 5, "原 fd 关闭后: dup fd 仍能读回完整数据");
    CHECK(memcmp(buf2, "hello", 5) == 0, "原 fd 关闭后: 内容正确");

    close(fd_dup);

    /* PART 3: dup3 到指定 fd 号 + O_CLOEXEC */

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd >= 0, "重新打开文件");

    int fd3 = dup3(fd, 30, 0);
    CHECK_RET(fd3, 30, "dup3 指定 fd=30 返回精确匹配");

    lseek(fd3, 0, SEEK_SET);
    char buf3[16] = {0};
    ssize_t n3 = read(fd3, buf3, 5);
    CHECK_RET(n3, 5, "dup3 fd=30: 读回完整数据");
    CHECK(memcmp(buf3, "hello", 5) == 0, "dup3 fd=30: 内容正确");
    close(fd3);

    int fd4 = dup3(fd, 31, O_CLOEXEC);
    CHECK_RET(fd4, 31, "dup3 O_CLOEXEC 指定 fd=31");

    int fd_flags = fcntl(fd4, F_GETFD);
    CHECK(fd_flags >= 0, "fcntl F_GETFD 成功");
    CHECK((fd_flags & FD_CLOEXEC) != 0, "dup3 O_CLOEXEC: F_GETFD 确认 FD_CLOEXEC 已设置");
    close(fd4);

    /* PART 4: fcntl F_DUPFD */

    int fd5 = fcntl(fd, F_DUPFD, 50);
    CHECK(fd5 >= 0, "fcntl F_DUPFD(50) 返回有效 fd");
    CHECK(fd5 >= 50, "fcntl F_DUPFD(50): 返回 fd >= 50 (bug 检测)");
    if (fd5 >= 0) {
        lseek(fd5, 0, SEEK_SET);
        char buf5[16];
        ssize_t n5 = read(fd5, buf5, 5);
        CHECK_RET(n5, 5, "F_DUPFD fd: 读回完整数据");
        CHECK(memcmp(buf5, "hello", 5) == 0, "F_DUPFD fd: 内容正确");

        int fl5 = fcntl(fd5, F_GETFD);
        if (fl5 >= 0) {
            CHECK((fl5 & FD_CLOEXEC) == 0, "F_DUPFD: 新 fd 无 FD_CLOEXEC");
        }
        close(fd5);
    }

    /* PART 5: fcntl F_DUPFD_CLOEXEC */

    int fd6 = fcntl(fd, F_DUPFD_CLOEXEC, 0);
    CHECK(fd6 >= 0, "fcntl F_DUPFD_CLOEXEC(0) 成功");
    if (fd6 >= 0) {
        int fl6 = fcntl(fd6, F_GETFD);
        CHECK(fl6 >= 0, "F_DUPFD_CLOEXEC: F_GETFD 成功");
        CHECK((fl6 & FD_CLOEXEC) != 0, "F_DUPFD_CLOEXEC: FD_CLOEXEC 已设置");
        close(fd6);
    }

    /* PART 6: fcntl F_SETFD/F_GETFD 状态转移 */

    CHECK_RET(fcntl(fd, F_SETFD, FD_CLOEXEC), 0, "F_SETFD -> FD_CLOEXEC: 设置成功");
    CHECK((fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0, "F_SETFD -> FD_CLOEXEC: F_GETFD 读回 == 1");

    CHECK_RET(fcntl(fd, F_SETFD, 0), 0, "F_SETFD -> 0: 清除 FD_CLOEXEC");
    CHECK((fcntl(fd, F_GETFD) & FD_CLOEXEC) == 0, "F_SETFD -> 0: F_GETFD 读回 == 0");

    /* PART 7: fcntl F_SETFL O_NONBLOCK */

    int pipefds[2];
    int pret = pipe(pipefds);
    CHECK_RET(pret, 0, "pipe 创建管道成功");

    if (pret == 0) {
        CHECK_RET(fcntl(pipefds[0], F_SETFL, O_NONBLOCK), 0, "F_SETFL -> O_NONBLOCK: 设置成功");

        int pfl = fcntl(pipefds[0], F_GETFL);
        CHECK(pfl >= 0, "F_GETFL 成功");
        CHECK((pfl & O_NONBLOCK) != 0, "F_GETFL 包含 O_NONBLOCK");

        char pbuf[4];
        errno = 0;
        CHECK_ERR(read(pipefds[0], pbuf, 1), EAGAIN, "O_NONBLOCK 空管道 read 返回 EAGAIN");

        close(pipefds[0]);
        close(pipefds[1]);
    }

    /* PART 8: fcntl F_SETFL O_APPEND */

    close(fd);
    create_temp_file_with_data(TMPFILE, "start");
    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd >= 0, "重新打开文件用于 O_APPEND 测试");

    CHECK_RET(fcntl(fd, F_SETFL, O_APPEND), 0, "F_SETFL -> O_APPEND: 设置成功");

    write(fd, "_middle", 7);
    off_t pos = lseek(fd, 0, SEEK_CUR);
    CHECK_RET(pos, 12, "O_APPEND: write 后 offset 在末尾");

    close(fd);

    /* PART 9: fcntl F_SETFL 清除标志 */

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR | O_APPEND);
    CHECK(fd >= 0, "打开文件带 O_APPEND 标志");

    int fl_before = fcntl(fd, F_GETFL);
    CHECK(fl_before >= 0, "F_GETFL 获取标志成功");

    CHECK_RET(fcntl(fd, F_SETFL, 0), 0, "F_SETFL -> 0: 清除所有修改标志");

    int fl_after = fcntl(fd, F_GETFL);
    CHECK(fl_after >= 0, "清除后 F_GETFL 成功");
    CHECK((fl_after & O_APPEND) == 0, "O_APPEND 已被清除");

    lseek(fd, 0, SEEK_SET);
    write(fd, "X", 1);
    lseek(fd, 0, SEEK_SET);
    char buf_append[16];
    read(fd, buf_append, 16);
    CHECK(buf_append[0] == 'X', "清除 O_APPEND 后: write 覆盖而非追加");

    close(fd);

    /* PART 10: fcntl F_GETFL 完整读回验证 */

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR | O_APPEND | O_NONBLOCK);
    CHECK(fd >= 0, "打开文件带多个标志");

    int fl_complete = fcntl(fd, F_GETFL);
    CHECK(fl_complete >= 0, "F_GETFL 成功");
    CHECK((fl_complete & O_ACCMODE) == O_RDWR, "F_GETFL: O_RDWR 正确");
    CHECK((fl_complete & O_APPEND) != 0, "F_GETFL: O_APPEND 已设置");
    CHECK((fl_complete & O_NONBLOCK) != 0, "F_GETFL: O_NONBLOCK 已设置");

    close(fd);

    /* PART 18: fcntl F_GETFL 基础验证 */

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd >= 0, "打开文件用于 F_GETFL 测试");

    int fl = fcntl(fd, F_GETFL);
    CHECK(fl >= 0, "fcntl F_GETFL 成功");
    CHECK((fl & O_ACCMODE) == O_RDWR, "F_GETFL: O_ACCMODE == O_RDWR(2)");

    close(fd);

    /* PART 19: 负向测试 */

    errno = 0;
    CHECK_ERR(dup(-1), EBADF, "dup(-1) -> EBADF");
    errno = 0;
    CHECK_ERR(dup(9999), EBADF, "dup(9999) -> EBADF");

    errno = 0;
    CHECK_ERR(dup3(5, 5, 0), EINVAL, "dup3(5,5,0) old==new -> EINVAL");

    errno = 0;
    int dup3_flags_ret = dup3(0, 40, 0xFF00);
    CHECK(dup3_flags_ret >= 0 || (dup3_flags_ret == -1 && errno == EINVAL),
          "dup3 未知 flags: 返回成功或 EINVAL 都接受");
    if (dup3_flags_ret >= 0) {
        close(dup3_flags_ret);
    }

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    if (fd >= 0) {
        errno = 0;
        CHECK_ERR(dup3(fd, -1, 0), EBADF, "dup3 newfd=-1 -> EBADF");
        close(fd);
    }

    errno = 0;
    CHECK_ERR(fcntl(-1, F_GETFD), EBADF, "fcntl(-1, F_GETFD) -> EBADF");

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    if (fd >= 0) {
        errno = 0;
        CHECK_ERR(fcntl(fd, F_DUPFD, -1), EINVAL, "fcntl F_DUPFD arg=-1 -> EINVAL");
        close(fd);
    }

    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    if (fd >= 0) {
        errno = 0;
        CHECK_ERR(fcntl(fd, 0xABCD), EINVAL, "fcntl 不支持的 cmd -> EINVAL");
        close(fd);
    }

    unlink(TMPFILE);

    return fail;
}
