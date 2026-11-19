#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>

/*
 * test-fallocate-memfd: weston 的 os_create_anonymous_file 用
 *   memfd_create + fallocate(mode=0) + mmap 来放编译好的 XKB keymap，
 *   失败就直接报 "failed to create anonymous file for keymap" 退出。
 *
 * 这个用例覆盖完整路径，并附带 PR #441 的回归 (mode!=0 → EOPNOTSUPP,
 * offset/len 取反 → EINVAL/EFBIG)。
 */

#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 0x0001U
#endif

static int sys_memfd_create(const char *name, unsigned int flags)
{
    return (int)syscall(SYS_memfd_create, name, flags);
}

int main(void)
{
    TEST_START("fallocate-memfd");

    int fd = sys_memfd_create("starry-test-memfd", MFD_CLOEXEC);
    CHECK(fd >= 0, "memfd_create 应成功");
    if (fd < 0) {
        TEST_DONE();
    }

    /* fallocate(mode=0) 应能扩张 memfd，weston 路径 */
    CHECK_RET(fallocate(fd, 0, 0, 4096), 0,
              "fallocate(memfd, 0, 0, 4096) 应返回 0");

    struct stat st;
    CHECK_RET(fstat(fd, &st), 0, "fstat memfd");
    CHECK(st.st_size == 4096, "memfd 大小应为 4096");

    /* 接 mmap+write，复现 weston 把 keymap blob 写进来的步骤 */
    void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "mmap memfd 应成功");
    if (p != MAP_FAILED) {
        ((char *)p)[0] = 'K';
        ((char *)p)[4095] = 'X';
        CHECK(((char *)p)[0] == 'K', "memfd mmap 写读应回得来 (head)");
        CHECK(((char *)p)[4095] == 'X', "memfd mmap 写读应回得来 (tail)");
        munmap(p, 4096);
    }

    /* 进一步扩张 */
    CHECK_RET(fallocate(fd, 0, 0, 8192), 0,
              "fallocate(memfd, 0, 0, 8192) 再次扩张");
    CHECK_RET(fstat(fd, &st), 0, "fstat 扩张后");
    CHECK(st.st_size == 8192, "memfd 应扩张到 8192");

    /* PR #441 回归：mode != 0 必须返回错误，不能静默成功 */
    errno = 0;
    long r = fallocate(fd, 0x01 /* FALLOC_FL_KEEP_SIZE */, 0, 4096);
    CHECK(r == -1 && (errno == EOPNOTSUPP || errno == ENOTSUP),
          "fallocate(mode=KEEP_SIZE) 应返回 EOPNOTSUPP");

    /* PR #441 回归：负的 offset 拒绝 */
    errno = 0;
    r = fallocate(fd, 0, -1, 4096);
    CHECK(r == -1 && errno == EINVAL,
          "fallocate(offset=-1) 应返回 EINVAL");

    /* PR #441 回归：len <= 0 拒绝 */
    errno = 0;
    r = fallocate(fd, 0, 0, 0);
    CHECK(r == -1 && errno == EINVAL,
          "fallocate(len=0) 应返回 EINVAL");

    close(fd);

    TEST_DONE();
}
