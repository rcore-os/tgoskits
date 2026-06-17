#include "test_framework.h"
#include "test_helpers.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <string.h>
#include <errno.h>

/* 独立的锁文件路径，与 flock 测试文件分开 */
#define RDLCK_FILE "/tmp/starry_test_rdlck_file"

int parts_fcntl_rdlck(void)
{
    int fd;
    int fd2;
    struct flock flk;

    /* PART 20: fcntl F_RDLCK 基础 — 本进程内基础行为检查 */

    unlink(RDLCK_FILE);
    create_temp_file_with_data(RDLCK_FILE, "rdlck_test_data");

    fd = openat(AT_FDCWD, RDLCK_FILE, O_RDWR);
    CHECK(fd >= 0, "Part 20: 打开锁文件");
    if (fd < 0) { unlink(RDLCK_FILE); return 1; }

    /* 断言 1: F_RDLCK 加锁成功 */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_RDLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;

    errno = 0;
    int ret = fcntl(fd, F_SETLK, &flk);
    int err = errno;
    CHECK_RET(ret, 0, "Part 20: fcntl F_RDLCK 设置读锁成功");
    (void)err;

    /* 断言 2: 第二个 fd 可以同时持有读锁（读锁共享） */
    fd2 = openat(AT_FDCWD, RDLCK_FILE, O_RDWR);
    CHECK(fd2 >= 0, "Part 20: 打开第二个 fd");
    if (fd2 < 0) { close(fd); unlink(RDLCK_FILE); return 1; }

    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_RDLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;

    errno = 0;
    ret = fcntl(fd2, F_SETLK, &flk);
    err = errno;
    CHECK_RET(ret, 0, "Part 20: 第二个 fd 获取读锁成功（共享）");

    /* 断言 3: 第二个 fd 加写锁 — 观察项（同进程内 fcntl 锁不阻塞同进程的其他 fd，这是 Linux 行为） */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_WRLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;

    errno = 0;
    ret = fcntl(fd2, F_SETLK, &flk);
    err = errno;
    if (ret == -1 && errno_is_lock_conflict(err)) {
        TEST_OBSERVE("Part 20: 同进程内读写互斥，加写锁失败（符合 POSIX）");
    } else {
        TEST_OBSERVE("Part 20: 同进程内读写不互斥，加写锁成功（Linux 行为）");
    }

    /* 断言 4: 释放第一个 fd 的读锁 */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_UNLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;

    errno = 0;
    ret = fcntl(fd, F_SETLK, &flk);
    err = errno;
    CHECK_RET(ret, 0, "Part 20: 释放第一个 fd 的读锁成功");

    /* 断言 5: 读锁释放后，第二个 fd 可以加写锁 */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_WRLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;

    errno = 0;
    ret = fcntl(fd2, F_SETLK, &flk);
    err = errno;
    CHECK_RET(ret, 0, "Part 20: 读锁释放后，加写锁成功");

    /* 清理 */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_UNLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;
    fcntl(fd2, F_SETLK, &flk);
    close(fd);
    close(fd2);
    unlink(RDLCK_FILE);

    /* PART 21: fcntl F_GETLK 读锁查询 — 观察项 */

    create_temp_file_with_data(RDLCK_FILE, "rdlck_query_data");
    fd = openat(AT_FDCWD, RDLCK_FILE, O_RDWR);
    CHECK(fd >= 0, "Part 21: 打开锁文件");

    /* 先持有读锁 */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_RDLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;
    fcntl(fd, F_SETLK, &flk);

    /* 观察项 1: F_GETLK 查询写锁，应返回 F_RDLCK */
    struct flock flk_query;
    memset(&flk_query, 0, sizeof(flk_query));
    flk_query.l_type = F_WRLCK;
    flk_query.l_whence = SEEK_SET;
    flk_query.l_start = 0;
    flk_query.l_len = 100;

    errno = 0;
    ret = fcntl(fd, F_GETLK, &flk_query);
    if (ret == 0 && flk_query.l_type == F_RDLCK) {
        TEST_OBSERVE("Part 21: F_GETLK 返回 F_RDLCK，符合预期");
    } else {
        TEST_OBSERVE("Part 21: F_GETLK 返回值与预期不同，需 baseline 验证");
    }

    /* 观察项 2: 观察 flk.l_pid */
    TEST_OBSERVE("Part 21: flk.l_pid = <观察值，不作为强断言>");

    /* 清理 */
    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_UNLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;
    fcntl(fd, F_SETLK, &flk);
    close(fd);
    unlink(RDLCK_FILE);

    /* PART 22: fcntl F_SETLK 读锁跨进程 — 强断言 */

    create_temp_file_with_data(RDLCK_FILE, "rdlck_xproc_data");
    fd = openat(AT_FDCWD, RDLCK_FILE, O_RDWR);
    CHECK(fd >= 0, "Part 22: 打开锁文件");
    if (fd < 0) { unlink(RDLCK_FILE); return 1; }

    /* 创建同步 pipe */
    int pipe_fds[2];
    if (sync_pipe_create(pipe_fds) != 0) {
        TEST_OBSERVE("Part 22: sync_pipe_create 失败，跳过跨进程测试");
        close(fd);
        unlink(RDLCK_FILE);
        return 0;
    }

    pid_t pid = fork();
    if (pid == 0) {
        /* 子进程 */
        close(pipe_fds[1]); /* 关闭写端 */

        /* 等待父进程通知 */
        if (sync_pipe_wait(&pipe_fds[0]) != 0) {
            close(pipe_fds[0]);
            exit(1);
        }

        /* 打开锁文件 */
        int fd_child = openat(AT_FDCWD, RDLCK_FILE, O_RDWR);
        if (fd_child < 0) {
            close(pipe_fds[0]);
            exit(1);
        }

        /* 断言 2: 子进程加读锁成功（共享） */
        struct flock flk_child;
        memset(&flk_child, 0, sizeof(flk_child));
        flk_child.l_type = F_RDLCK;
        flk_child.l_whence = SEEK_SET;
        flk_child.l_start = 0;
        flk_child.l_len = 100;

        errno = 0;
        int ret_child = fcntl(fd_child, F_SETLK, &flk_child);
        if (ret_child != 0) {
            close(fd_child);
            close(pipe_fds[0]);
            exit(1);
        }

        /* 断言 3: 子进程加写锁应失败（读写互斥） */
        memset(&flk_child, 0, sizeof(flk_child));
        flk_child.l_type = F_WRLCK;
        flk_child.l_whence = SEEK_SET;
        flk_child.l_start = 0;
        flk_child.l_len = 100;

        errno = 0;
        ret_child = fcntl(fd_child, F_SETLK, &flk_child);
        int err_child = errno;
        if (ret_child == -1 && errno_is_lock_conflict(err_child)) {
            /* 通过 */
            close(fd_child);
            close(pipe_fds[0]);
            exit(0);
        } else {
            /* 失败 */
            close(fd_child);
            close(pipe_fds[0]);
            exit(1);
        }
    } else if (pid > 0) {
        /* 父进程 */
        close(pipe_fds[0]); /* 关闭读端 */

        /* 父进程加读锁 */
        memset(&flk, 0, sizeof(flk));
        flk.l_type = F_RDLCK;
        flk.l_whence = SEEK_SET;
        flk.l_start = 0;
        flk.l_len = 100;

        errno = 0;
        ret = fcntl(fd, F_SETLK, &flk);
        err = errno;
        CHECK_RET(ret, 0, "Part 22: 父进程加读锁成功");

        /* 通知子进程 */
        sync_pipe_signal(&pipe_fds[1]);
        close(pipe_fds[1]);

        /* 等待子进程 */
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "Part 22: 子进程正确检测到读锁共享和写锁冲突");

        /* 清理 */
        memset(&flk, 0, sizeof(flk));
        flk.l_type = F_UNLCK;
        flk.l_whence = SEEK_SET;
        flk.l_start = 0;
        flk.l_len = 100;
        fcntl(fd, F_SETLK, &flk);
        close(fd);
        unlink(RDLCK_FILE);
    }

    return 0;
}
