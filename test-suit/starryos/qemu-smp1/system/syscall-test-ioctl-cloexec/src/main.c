#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <sys/ioctl.h>
#include <unistd.h>

/*
 * ioctl(FIOCLEX) / ioctl(FIONCLEX) 回归测试 — 对应 PR #1168 (fix-tui-procfs-ioctl)。
 *
 * 触发背景 (为什么写这个测例):
 *   ncurses / CPython (glances/htop) 在每个新开 fd 上用 ioctl(FIOCLEX) 设置
 *   close-on-exec —— 这是 fcntl(fd, F_SETFD, FD_CLOEXEC) 的 ioctl 拼写。starry
 *   此前只在 tty 设备路径里识别 ioctl, 普通(非 tty) fd 上 FIOCLEX 落到通用
 *   分支 -> "Unsupported ioctl command: 21585" 并被拒绝, FD_CLOEXEC 没被设置。
 *
 * man 2 ioctl_tty / linux ioctl 通用语义:
 *   FIOCLEX  (0x5451): set close-on-exec  == fcntl(fd, F_SETFD, FD_CLOEXEC)
 *   FIONCLEX (0x5450): clear close-on-exec == fcntl(fd, F_SETFD, 0)
 *   Linux 对**任意** fd 都通用实现 (不限于 tty)。
 *
 * starry 实现 (kernel/src/syscall/fs/ctl.rs sys_ioctl):
 *   FIOCLEX/FIONCLEX 在 ioctl 入口直接改 FD_TABLE 项的 cloexec 位 (镜像 fcntl
 *   F_SETFD), 任意 fd 接受; 无效 fd 由入口 get_file_like(fd) 先返回 EBADF。
 *
 * 修复前: ioctl(FIOCLEX) 在普通 fd 上失败 / FD_CLOEXEC 不变 -> 本测例 FAIL。
 * 修复后: FD_CLOEXEC 经 fcntl(F_GETFD) 可见地被 set/clear -> PASS。
 *
 * 用非 tty fd: pipe (与 test_ioctl_fionbio_int 同源), 不依赖 /dev/tty 是否存在。
 */

#ifndef FIOCLEX
#define FIOCLEX 0x5451
#endif
#ifndef FIONCLEX
#define FIONCLEX 0x5450
#endif

int main(void)
{
    TEST_START("ioctl FIOCLEX/FIONCLEX -> FD_CLOEXEC");

    int p[2];
    CHECK_RET(pipe(p), 0, "pipe() 创建非 tty fd 成功");

    /* 新建 fd 默认 close-on-exec 关闭 */
    int fd0 = fcntl(p[0], F_GETFD);
    CHECK(fd0 >= 0, "F_GETFD 初始读取成功");
    CHECK((fd0 & FD_CLOEXEC) == 0, "新 pipe fd 初始 FD_CLOEXEC 未设置");

    /* ---- FIOCLEX 应 set FD_CLOEXEC (等价 fcntl F_SETFD FD_CLOEXEC) ---- */
    CHECK_RET(ioctl(p[0], FIOCLEX), 0, "ioctl(FIOCLEX) 在非 tty fd 上返回 0");
    int fd1 = fcntl(p[0], F_GETFD);
    CHECK(fd1 >= 0, "F_GETFD (FIOCLEX 后) 成功");
    CHECK((fd1 & FD_CLOEXEC) != 0, "FIOCLEX 后 FD_CLOEXEC 被设置");

    /* ---- FIONCLEX 应 clear FD_CLOEXEC ---- */
    CHECK_RET(ioctl(p[0], FIONCLEX), 0, "ioctl(FIONCLEX) 返回 0");
    int fd2 = fcntl(p[0], F_GETFD);
    CHECK(fd2 >= 0, "F_GETFD (FIONCLEX 后) 成功");
    CHECK((fd2 & FD_CLOEXEC) == 0, "FIONCLEX 后 FD_CLOEXEC 被清除");

    /* ---- 与 fcntl(F_SETFD) 互证: 两条路径应一致 ---- */
    CHECK_RET(fcntl(p[0], F_SETFD, FD_CLOEXEC), 0, "fcntl(F_SETFD, FD_CLOEXEC) 成功");
    CHECK((fcntl(p[0], F_GETFD) & FD_CLOEXEC) != 0, "fcntl 设置后 FD_CLOEXEC 可见");
    CHECK_RET(ioctl(p[0], FIONCLEX), 0, "ioctl(FIONCLEX) 再次清除 fcntl 设的位");
    CHECK((fcntl(p[0], F_GETFD) & FD_CLOEXEC) == 0, "FIONCLEX 清除 fcntl 设的位生效");

    close(p[0]);
    close(p[1]);

    /* ---- 无效 fd 应返回 EBADF (入口 get_file_like 先校验) ---- */
    CHECK_ERR(ioctl(-1, FIOCLEX), EBADF, "ioctl(FIOCLEX) 无效 fd=-1 返回 EBADF");
    CHECK_ERR(ioctl(p[0], FIOCLEX), EBADF, "ioctl(FIOCLEX) 已关闭 fd 返回 EBADF");

    TEST_DONE();
}
