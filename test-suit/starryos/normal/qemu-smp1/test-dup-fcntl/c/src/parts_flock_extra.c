#include "test_framework.h"
#include "test_helpers.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <string.h>
#include <errno.h>

/* 每个 Part 使用独立的临时文件 */
#define FLOCK_EXTRA_FILE1 "/tmp/starry_test_flock_extra1"
#define FLOCK_EXTRA_FILE2 "/tmp/starry_test_flock_extra2"
#define FLOCK_EXTRA_FILE3 "/tmp/starry_test_flock_extra3"
#define FLOCK_EXTRA_FILE4 "/tmp/starry_test_flock_extra4"

int parts_flock_extra(void)
{
    int fd;
    int dupfd;
    int fd_reopen;
    int ret;

    /* PART 23: 同一 open file description 的 flock 共享 */

    unlink(FLOCK_EXTRA_FILE1);
    create_temp_file_with_data(FLOCK_EXTRA_FILE1, "flock_shared_data");

    fd = openat(AT_FDCWD, FLOCK_EXTRA_FILE1, O_RDWR);
    CHECK(fd >= 0, "Part 23: 打开文件");
    if (fd < 0) { unlink(FLOCK_EXTRA_FILE1); return 1; }

    /* 断言 1: fd 加 LOCK_EX 成功 */
    errno = 0;
    ret = flock(fd, LOCK_EX);
    int err = errno;
    CHECK_RET(ret, 0, "Part 23: flock(LOCK_EX) 获取排他锁成功");

    /* 断言 2: dup(fd) 后 safe_close(&fd)，保留 dupfd 引用 */
    dupfd = dup(fd);
    CHECK(dupfd >= 0, "Part 23: dup(fd) 成功");
    if (dupfd < 0) { close(fd); unlink(FLOCK_EXTRA_FILE1); return 1; }
    safe_close(&fd);

    /* 断言 3: reopen 同文件，LOCK_EX|LOCK_NB 失败（不同 OFD 冲突） */
    fd_reopen = openat(AT_FDCWD, FLOCK_EXTRA_FILE1, O_RDWR);
    CHECK(fd_reopen >= 0, "Part 23: reopen 文件");
    if (fd_reopen < 0) { close(dupfd); unlink(FLOCK_EXTRA_FILE1); return 1; }

    errno = 0;
    ret = flock(fd_reopen, LOCK_EX | LOCK_NB);
    err = errno;
    CHECK_RET(ret, -1, "Part 23: 不同 OFD 尝试加锁应失败");
    CHECK_TRUE(errno_is_wouldblock(err), "Part 23: errno 为 EWOULDBLOCK 或 EAGAIN");

    /* 断言 4: safe_close(&dupfd) 后，reopen LOCK_EX|LOCK_NB 成功 */
    safe_close(&dupfd);

    errno = 0;
    ret = flock(fd_reopen, LOCK_EX | LOCK_NB);
    err = errno;
    CHECK_RET(ret, 0, "Part 23: 最后一个引用关闭后，锁释放");

    /* 清理 */
    flock(fd_reopen, LOCK_UN);
    close(fd_reopen);
    unlink(FLOCK_EXTRA_FILE1);

    /* PART 24: flock close 自动释放 */

    unlink(FLOCK_EXTRA_FILE2);
    create_temp_file_with_data(FLOCK_EXTRA_FILE2, "flock_close_data");

    /* 断言 1: fd 加 LOCK_EX 后 safe_close(&fd) */
    fd = openat(AT_FDCWD, FLOCK_EXTRA_FILE2, O_RDWR);
    CHECK(fd >= 0, "Part 24: 打开文件");
    if (fd < 0) { unlink(FLOCK_EXTRA_FILE2); return 1; }

    ret = flock(fd, LOCK_EX);
    CHECK_RET(ret, 0, "Part 24: flock(LOCK_EX) 成功");
    safe_close(&fd);

    /* 断言 2: reopen LOCK_EX|LOCK_NB 成功（锁已释放） */
    fd_reopen = openat(AT_FDCWD, FLOCK_EXTRA_FILE2, O_RDWR);
    CHECK(fd_reopen >= 0, "Part 24: reopen 文件");

    errno = 0;
    ret = flock(fd_reopen, LOCK_EX | LOCK_NB);
    err = errno;
    CHECK_RET(ret, 0, "Part 24: close 最后引用后锁释放，重新加锁成功");

    /* 清理 */
    flock(fd_reopen, LOCK_UN);
    close(fd_reopen);
    unlink(FLOCK_EXTRA_FILE2);

    /* PART 25: 不同 open file description 的 flock 冲突 */

    unlink(FLOCK_EXTRA_FILE3);
    create_temp_file_with_data(FLOCK_EXTRA_FILE3, "flock_conflict_data");

    int fd1 = openat(AT_FDCWD, FLOCK_EXTRA_FILE3, O_RDWR);
    CHECK(fd1 >= 0, "Part 25: 打开文件 fd1");
    if (fd1 < 0) { unlink(FLOCK_EXTRA_FILE3); return 1; }

    /* 断言 1: fd1 加 LOCK_EX 成功 */
    errno = 0;
    ret = flock(fd1, LOCK_EX);
    err = errno;
    CHECK_RET(ret, 0, "Part 25: flock(LOCK_EX) 获取排他锁成功");

    /* 断言 2: fd2 加 LOCK_SH|LOCK_NB 失败（不同 OFD 冲突） */
    int fd2 = openat(AT_FDCWD, FLOCK_EXTRA_FILE3, O_RDWR);
    CHECK(fd2 >= 0, "Part 25: 打开文件 fd2");
    if (fd2 < 0) { flock(fd1, LOCK_UN); close(fd1); unlink(FLOCK_EXTRA_FILE3); return 1; }

    errno = 0;
    ret = flock(fd2, LOCK_SH | LOCK_NB);
    err = errno;
    CHECK_RET(ret, -1, "Part 25: 不同 OFD 尝试 LOCK_SH 应失败");
    CHECK_TRUE(errno_is_wouldblock(err), "Part 25: errno 为 EWOULDBLOCK 或 EAGAIN");

    /* 断言 3: flock(LOCK_UN) 后，fd2 LOCK_EX|LOCK_NB 成功 */
    flock(fd1, LOCK_UN);

    errno = 0;
    ret = flock(fd2, LOCK_EX | LOCK_NB);
    err = errno;
    CHECK_RET(ret, 0, "Part 25: 排他锁释放后，fd2 加锁成功");

    /* 清理 */
    flock(fd2, LOCK_UN);
    close(fd1);
    close(fd2);
    unlink(FLOCK_EXTRA_FILE3);

    /* PART 26: LOCK_SH -> EX 升级失败 — 观察项 */

    unlink(FLOCK_EXTRA_FILE4);
    create_temp_file_with_data(FLOCK_EXTRA_FILE4, "flock_upgrade_data");

    fd = openat(AT_FDCWD, FLOCK_EXTRA_FILE4, O_RDWR);
    CHECK(fd >= 0, "Part 26: 打开文件");
    if (fd < 0) { unlink(FLOCK_EXTRA_FILE4); return 1; }

    /* 观察项 1: flock(LOCK_SH) 成功 */
    errno = 0;
    ret = flock(fd, LOCK_SH);
    if (ret == 0) {
        TEST_OBSERVE("Part 26: flock(LOCK_SH) 成功");
    } else {
        TEST_OBSERVE("Part 26: flock(LOCK_SH) 失败，需 baseline 验证");
        close(fd);
        unlink(FLOCK_EXTRA_FILE4);
        return 0;
    }

    /* 观察项 2: flock(LOCK_EX|LOCK_NB) 失败（SH->EX 升级失败） */
    errno = 0;
    ret = flock(fd, LOCK_EX | LOCK_NB);
    int err2 = errno;
    if (ret == -1 && errno_is_wouldblock(err2)) {
        TEST_OBSERVE("Part 26: LOCK_SH->EX 升级失败，符合预期");
    } else {
        TEST_OBSERVE("Part 26: LOCK_SH->EX 升级结果与预期不同，需 baseline 验证");
    }

    /* 清理 */
    flock(fd, LOCK_UN);
    close(fd);
    unlink(FLOCK_EXTRA_FILE4);

    return 0;
}
