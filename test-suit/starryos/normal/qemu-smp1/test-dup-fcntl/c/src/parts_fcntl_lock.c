#include "test_framework.h"
#include "test_helpers.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <string.h>
#include <errno.h>

int parts_fcntl_lock(void)
{
    int fd;
    struct flock flk;

    /* PART 15: fcntl F_SETLK 写锁与 F_GETLK 查询 */

    create_temp_file_with_data(TMPFILE, "fcntl_lock_data");
    fd = openat(AT_FDCWD, TMPFILE, O_RDWR);
    CHECK(fd >= 0, "fcntl 文件锁测试: 打开文件");

    memset(&flk, 0, sizeof(flk));
    flk.l_type = F_WRLCK;
    flk.l_whence = SEEK_SET;
    flk.l_start = 0;
    flk.l_len = 100;
    flk.l_pid = 0;

    CHECK_RET(fcntl(fd, F_SETLK, &flk), 0, "fcntl F_SETLK(F_WRLCK) 设置写锁成功");

    struct flock flk_query;
    memset(&flk_query, 0, sizeof(flk_query));
    flk_query.l_type = F_WRLCK;
    flk_query.l_whence = SEEK_SET;
    flk_query.l_start = 0;
    flk_query.l_len = 100;

    CHECK_RET(fcntl(fd, F_GETLK, &flk_query), 0, "fcntl F_GETLK 查询锁状态");
    CHECK(flk_query.l_type == F_UNLCK, "F_GETLK: 同进程查询返回 F_UNLCK");

    flk.l_type = F_UNLCK;
    fcntl(fd, F_SETLK, &flk);

    /* PART 16: fcntl F_SETLK 跨进程写锁冲突 */

    flk.l_type = F_WRLCK;
    CHECK_RET(fcntl(fd, F_SETLK, &flk), 0, "父进程: F_SETLK(F_WRLCK) 设置写锁");

    pid_t pid = fork();
    if (pid == 0) {
        int fd_child = openat(AT_FDCWD, TMPFILE, O_RDWR);
        if (fd_child >= 0) {
            struct flock flk_child;
            memset(&flk_child, 0, sizeof(flk_child));
            flk_child.l_type = F_WRLCK;
            flk_child.l_whence = SEEK_SET;
            flk_child.l_start = 0;
            flk_child.l_len = 100;

            errno = 0;
            int ret = fcntl(fd_child, F_SETLK, &flk_child);
            if (ret == -1 && (errno == EACCES || errno == EAGAIN)) {
                exit(0);
            } else {
                exit(1);
            }
            close(fd_child);
        }
        exit(1);
    } else if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "fcntl F_SETLK 跨进程冲突: 子进程正确检测到写锁冲突");
    }

    flk.l_type = F_UNLCK;
    fcntl(fd, F_SETLK, &flk);

    /* PART 17: fcntl F_SETLKW 阻塞等待 */

    pid = fork();
    if (pid == 0) {
        struct flock flk_child;
        memset(&flk_child, 0, sizeof(flk_child));
        flk_child.l_type = F_WRLCK;
        flk_child.l_whence = SEEK_SET;
        flk_child.l_start = 0;
        flk_child.l_len = 0;

        fcntl(fd, F_SETLKW, &flk_child);
        sleep(1);

        flk_child.l_type = F_UNLCK;
        fcntl(fd, F_SETLKW, &flk_child);
        exit(0);
    } else if (pid > 0) {
        sleep(1);

        struct flock flk_parent;
        memset(&flk_parent, 0, sizeof(flk_parent));
        flk_parent.l_type = F_WRLCK;
        flk_parent.l_whence = SEEK_SET;
        flk_parent.l_start = 0;
        flk_parent.l_len = 0;

        CHECK_RET(fcntl(fd, F_SETLKW, &flk_parent), 0, "F_SETLKW: 等待后成功获取锁");

        flk_parent.l_type = F_UNLCK;
        fcntl(fd, F_SETLK, &flk_parent);

        wait(NULL);
    }

    close(fd);

    return 0;
}
