#include "test_framework.h"
#include "test_helpers.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <sys/wait.h>
#include <string.h>
#include <errno.h>

int parts_flock_basic(void)
{
    int fd;

    /* PART 11: flock LOCK_SH 共享锁 */

    create_temp_file_with_data(TMPFILE, "flock_test_data");
    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd >= 0, "flock 测试: 打开文件");

    CHECK_RET(flock(fd, LOCK_SH), 0, "flock(LOCK_SH) 获取共享锁成功");

    int fd2_lock = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd2_lock >= 0, "flock: 打开第二个 fd");

    int ret2 = flock(fd2_lock, LOCK_SH | LOCK_NB);
    CHECK(ret2 == 0, "flock: 第二个 fd 也能获取共享锁");

    flock(fd, LOCK_UN);
    flock(fd2_lock, LOCK_UN);
    close(fd2_lock);

    /* PART 12: flock LOCK_EX 排他锁冲突 */

    CHECK_RET(flock(fd, LOCK_EX), 0, "flock(LOCK_EX) 获取排他锁成功");

    fd2_lock = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd2_lock >= 0, "flock: 打开第二个 fd 用于冲突测试");

    errno = 0;
    int ret_ex = flock(fd2_lock, LOCK_SH | LOCK_NB);
    CHECK(ret_ex == -1, "flock: 已有排他锁时，LOCK_SH|LOCK_NB 失败");
    CHECK(errno == EWOULDBLOCK, "flock 冲突: errno == EWOULDBLOCK");

    flock(fd, LOCK_UN);
    close(fd2_lock);

    /* PART 13: flock LOCK_UN 释放锁 */

    CHECK_RET(flock(fd, LOCK_EX), 0, "flock: 获取排他锁");

    flock(fd, LOCK_UN);

    fd2_lock = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd2_lock >= 0, "flock: 打开第二个 fd 验证锁释放");

    ret2 = flock(fd2_lock, LOCK_EX | LOCK_NB);
    CHECK(ret2 == 0, "flock LOCK_UN 后: 第二个 fd 可以获取排他锁");

    flock(fd2_lock, LOCK_UN);
    close(fd2_lock);

    /* PART 14: flock 进程继承 */

    CHECK_RET(flock(fd, LOCK_EX), 0, "flock: 父进程获取排他锁");

    pid_t pid = fork();
    if (pid == 0) {
        char buf_inherit[16];
        lseek(fd, 0, SEEK_SET);
        ssize_t n_inherit = read(fd, buf_inherit, 5);
        exit(n_inherit == 5 ? 0 : 1);
    } else if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "flock: 子进程继承了锁，可以操作文件");
    }

    close(fd);

    return 0;
}
