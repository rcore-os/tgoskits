/*
 * test-modern-fd-family — memfd_create 与 pidfd_open / pidfd_send_signal / pidfd_getfd
 *
 * 对照 Linux man 2 语义；单可执行文件供 syscall grouped 流水线顺序执行。
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MFD_CLOEXEC
#define MFD_CLOEXEC 1u
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 2u
#endif
/* Linux uapi fcntl seals: may be missing from some guest libc headers. */
#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_GET_SEALS
#define F_GET_SEALS 1034
#endif
#ifndef F_SEAL_SEAL
#define F_SEAL_SEAL 0x0001
#endif
#ifndef F_SEAL_SHRINK
#define F_SEAL_SHRINK 0x0002
#endif
#ifndef F_SEAL_GROW
#define F_SEAL_GROW 0x0004
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif
/* Linux uapi linux/memfd.h (not always in guest libc headers). */
#ifndef MFD_HUGETLB
#define MFD_HUGETLB 4u
#endif

/* man 2 memfd_create: name up to 249 bytes excluding terminating NUL. */
#define MFD_NAME_MAX_EXCL_NUL 249

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h> for this arch/toolchain"
#endif
#ifndef __NR_pidfd_send_signal
#error "__NR_pidfd_send_signal required from <sys/syscall.h>"
#endif
#ifndef __NR_pidfd_getfd
#error "__NR_pidfd_getfd required from <sys/syscall.h>"
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int x_pidfd_send_signal(int pidfd, int sig, siginfo_t *info, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_send_signal, pidfd, sig, info, flags);
}

static int x_pidfd_getfd(int pidfd, int targetfd, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_getfd, pidfd, targetfd, flags);
}

/* ---- memfd_create ---- */

static int get_cloexec(int fd)
{
    int fl = fcntl(fd, F_GETFD);
    if (fl < 0) {
        return -1;
    }
    return !!(fl & FD_CLOEXEC);
}

static void test_memfd_normal(void)
{
    printf("--- memfd_create 正常路径 ---\n");

    errno = 0;
    int fd = memfd_create("starry_memfd", 0);
    CHECK(fd >= 0, "memfd_create 返回非负 fd");
    if (fd < 0) {
        return;
    }

    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate(fd, 4096) 成功");

    const char *msg = "memfd";
    size_t len = strlen(msg);
    ssize_t w = write(fd, msg, len);
    CHECK(w == (ssize_t)len, "write 长度正确");

    CHECK_RET(fsync(fd), 0, "fsync 后可见写入");

    CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "lseek SEEK_SET 0");

    char buf[32] = {0};
    ssize_t r = read(fd, buf, len);
    CHECK(r == (ssize_t)len && memcmp(buf, msg, len) == 0, "read 回写内容与长度一致");

    CHECK_RET(close(fd), 0, "close memfd");
}

static void test_memfd_empty_name(void)
{
    printf("--- memfd_create 空名字 ---\n");

    errno = 0;
    int fd = memfd_create("", 0);
    CHECK(fd >= 0, "空字符串 name 允许 (Linux)");
    if (fd >= 0) {
        CHECK_RET(close(fd), 0, "close memfd");
    }
}

static void test_memfd_errors(void)
{
    printf("--- memfd_create 错误路径 ---\n");

    CHECK_ERR(memfd_create(NULL, 0), EFAULT, "NULL name -> EFAULT");

    errno = 0;
    int bad = memfd_create("x", 0xFFFFFFFFu);
    CHECK(bad == -1 && errno == EINVAL, "非法 flags -> EINVAL");
}

static void test_memfd_name_limits(void)
{
    printf("--- memfd_create 名字长度 (man: 249 bytes excl. NUL) ---\n");

    char name249[MFD_NAME_MAX_EXCL_NUL + 2];
    memset(name249, 'b', (size_t)MFD_NAME_MAX_EXCL_NUL);
    name249[MFD_NAME_MAX_EXCL_NUL] = '\0';

    errno = 0;
    int ok = memfd_create(name249, 0);
    CHECK(ok >= 0, "249 字节 name 边界成功");
    if (ok >= 0) {
        CHECK_RET(close(ok), 0, "close memfd (249-byte name)");
    }

    char name250[MFD_NAME_MAX_EXCL_NUL + 2 + 1];
    memset(name250, 'a', (size_t)MFD_NAME_MAX_EXCL_NUL + 1u);
    name250[MFD_NAME_MAX_EXCL_NUL + 1] = '\0';
    CHECK_ERR(memfd_create(name250, 0), EINVAL, "250 字节 name -> EINVAL");
}

static void test_memfd_hugetlb_and_reserved_flags(void)
{
    printf("--- memfd_create HUGETLB / 保留位 (对照 man ERRORS) ---\n");

    /* MFD_HUGETLB 未支持时应失败；与 MFD_ALLOW_SEALING 同设时 man 要求 EINVAL。 */
    CHECK_ERR(memfd_create("hugetlb_only", MFD_HUGETLB), EINVAL,
              "MFD_HUGETLB 单独 -> EINVAL (当前未实现)");

    CHECK_ERR(memfd_create("hugetlb_seal", MFD_HUGETLB | MFD_ALLOW_SEALING), EINVAL,
              "MFD_HUGETLB|MFD_ALLOW_SEALING -> EINVAL");

    CHECK_ERR(memfd_create("rsvd", 1u << 31), EINVAL, "保留/未知 flag 高位 -> EINVAL");
}

static void test_memfd_flags(void)
{
    printf("--- memfd_create 标志位 ---\n");

    errno = 0;
    int fd = memfd_create("cloexec_fd", MFD_CLOEXEC);
    CHECK(fd >= 0, "MFD_CLOEXEC 创建成功");
    if (fd >= 0) {
        CHECK(get_cloexec(fd) == 1, "MFD_CLOEXEC 后 FD_CLOEXEC 置位");
        close(fd);
    }

    errno = 0;
    fd = memfd_create("noseal", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "MFD_ALLOW_SEALING 创建成功");
    if (fd >= 0) {
        int seals = fcntl(fd, F_GET_SEALS);
        CHECK(seals >= 0 && seals == 0, "F_GET_SEALS 初始为 0");

        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "F_ADD_SEALS(F_SEAL_WRITE) 成功");
        seals = fcntl(fd, F_GET_SEALS);
        CHECK(seals >= 0 && (seals & F_SEAL_WRITE) != 0, "F_GET_SEALS 包含 F_SEAL_WRITE");

        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_SEAL), 0, "F_ADD_SEALS(F_SEAL_SEAL) 成功");
        CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW), EPERM,
                  "F_SEAL_SEAL 后继续 ADD_SEALS -> EPERM");
        close(fd);
    }

    errno = 0;
    fd = memfd_create("both", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "MFD_CLOEXEC|MFD_ALLOW_SEALING 创建成功");
    if (fd >= 0) {
        CHECK(get_cloexec(fd) == 1, "组合标志下 FD_CLOEXEC 置位");
        close(fd);
    }
}

static void test_memfd_seal_enforcement(void)
{
    printf("--- memfd seals enforcement (F_SEAL_*) ---\n");

    /* F_SEAL_WRITE: write(2) should fail with EPERM. */
    errno = 0;
    int fd = memfd_create("seal_write", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_write) 成功");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 初始 4KiB");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");

        errno = 0;
        ssize_t w = write(fd, "x", 1);
        CHECK(w == -1 && errno == EPERM, "F_SEAL_WRITE 后 write -> EPERM");

        /* F_SEAL_WRITE: mmap(MAP_SHARED|PROT_WRITE) should fail with EPERM. */
        errno = 0;
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK(p == MAP_FAILED && errno == EPERM, "F_SEAL_WRITE 后 shared writable mmap -> EPERM");
        if (p != MAP_FAILED) {
            munmap(p, 4096);
        }

        /* Linux: MAP_PRIVATE|PROT_WRITE stays allowed (COW); does not mutate the memfd object. */
        errno = 0;
        p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
        CHECK(p != MAP_FAILED, "F_SEAL_WRITE 后 private writable mmap 仍成功");
        if (p != MAP_FAILED) {
            CHECK_RET(munmap(p, 4096), 0, "munmap private map");
        }

        close(fd);
    }

    /* F_SEAL_GROW: ftruncate that grows should fail with EPERM. */
    errno = 0;
    fd = memfd_create("seal_grow", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_grow) 成功");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 初始 4KiB");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW), 0, "ADD_SEALS(F_SEAL_GROW)");
        CHECK_ERR(ftruncate(fd, 8192), EPERM, "F_SEAL_GROW 后 grow ftruncate -> EPERM");
        CHECK_RET(ftruncate(fd, 4096), 0, "F_SEAL_GROW 后同尺寸 ftruncate 仍成功");
        errno = 0;
        ssize_t pw = pwrite(fd, "z", 1, 8190);
        CHECK(pw == -1 && errno == EPERM, "F_SEAL_GROW 后 pwrite 隐式扩展 -> EPERM");
        close(fd);
    }

    /* F_SEAL_SHRINK: ftruncate that shrinks should fail with EPERM. */
    errno = 0;
    fd = memfd_create("seal_shrink", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_shrink) 成功");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 初始 4KiB");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_SHRINK), 0, "ADD_SEALS(F_SEAL_SHRINK)");
        CHECK_ERR(ftruncate(fd, 2048), EPERM, "F_SEAL_SHRINK 后 shrink ftruncate -> EPERM");
        CHECK_RET(ftruncate(fd, 4096), 0, "F_SEAL_SHRINK 后同尺寸 ftruncate 仍成功");
        close(fd);
    }
}

/* ---- pidfd_open ---- */

static void test_pidfd_open_self(void)
{
    printf("--- pidfd_open 正常路径 ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid(), 0) 返回 fd");
    if (pfd >= 0) {
        CHECK_RET(close(pfd), 0, "close pidfd");
    }
}

static void test_pidfd_open_errors(void)
{
    printf("--- pidfd_open 错误路径 ---\n");

    errno = 0;
    pid_t stale = (pid_t)999999001;
    if (stale <= 0) {
        stale = (pid_t)2147483644;
    }
    int r = x_pidfd_open(stale, 0);
    CHECK(r == -1 && (errno == ESRCH || errno == EINVAL),
          "不存在 pid -> ESRCH 或 EINVAL");

    CHECK_ERR(x_pidfd_open(getpid(), 0xFFFFFFFFu), EINVAL, "非法 flags -> EINVAL");
}

/* ---- pidfd_send_signal ---- */

static void test_pidfd_send_signal_paths(void)
{
    printf("--- pidfd_send_signal ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid()) 成功");
    if (pfd < 0) {
        return;
    }

    CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 1u), EINVAL,
              "flags 非零 -> EINVAL");

    CHECK_RET(x_pidfd_send_signal(pfd, 0, NULL, 0), 0, "signo==0 空 info 成功 (no-op)");

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = SIG_IGN;
    CHECK_RET(sigaction(SIGUSR1, &sa, NULL), 0, "忽略 SIGUSR1");

    CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), 0,
              "SIGUSR1 + NULL info 成功 (已忽略)");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

/* ---- pidfd_getfd ---- */

static void test_pidfd_getfd_flags(void)
{
    printf("--- pidfd_getfd 非法 flags ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(self) 成功");
    if (pfd < 0) {
        return;
    }

    CHECK_ERR(x_pidfd_getfd(pfd, 0, 1u), EINVAL, "flags 非零 -> EINVAL");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_pidfd_getfd_cross_process(void)
{
    printf("--- pidfd_getfd 跨进程 pipe ---\n");

    int c2p[2];
    int p2c[2];
    CHECK_RET(pipe(c2p), 0, "pipe c2p");
    CHECK_RET(pipe(p2c), 0, "pipe p2c");

    pid_t cpid = fork();
    CHECK(cpid >= 0, "fork 成功");

    if (cpid == 0) {
        close(c2p[0]);
        close(p2c[1]);

        int data[2];
        if (pipe(data) != 0) {
            _exit(21);
        }

        int rd = data[0];
        int wr = data[1];
        if (write(c2p[1], &wr, sizeof(wr)) != (ssize_t)sizeof(wr)) {
            _exit(22);
        }

        char ack;
        if (read(p2c[0], &ack, 1) != 1) {
            _exit(23);
        }

        char buf[8] = {0};
        ssize_t n = read(rd, buf, sizeof(buf) - 1);
        close(rd);
        close(wr);
        close(c2p[1]);
        close(p2c[0]);
        if (n != 2 || buf[0] != 'H' || buf[1] != 'I') {
            _exit(24);
        }
        _exit(0);
    }

    close(c2p[1]);
    close(p2c[0]);

    int child_wr = -1;
    CHECK((ssize_t)read(c2p[0], &child_wr, sizeof(child_wr)) == (ssize_t)sizeof(child_wr),
          "读取子进程 target fd 编号");
    close(c2p[0]);

    errno = 0;
    int pidfd = x_pidfd_open(cpid, 0);
    CHECK(pidfd >= 0, "pidfd_open(child) 成功");
    if (pidfd < 0) {
        char z = 0;
        write(p2c[1], &z, 1);
        waitpid(cpid, NULL, 0);
        close(p2c[1]);
        return;
    }

    errno = 0;
    int dupfd = x_pidfd_getfd(pidfd, child_wr, 0);
    CHECK(dupfd >= 0, "pidfd_getfd 成功");
    if (dupfd < 0) {
        char z = 0;
        write(p2c[1], &z, 1);
        waitpid(cpid, NULL, 0);
        close(pidfd);
        close(p2c[1]);
        return;
    }

    const char *out = "HI";
    CHECK((ssize_t)write(dupfd, out, 2) == 2, "向 dup 的 pipe 写端写入");

    char go = 1;
    CHECK_RET(write(p2c[1], &go, 1), 1, "通知子进程开始读");

    close(dupfd);
    close(pidfd);
    close(p2c[1]);

    int st = 0;
    CHECK_RET(waitpid(cpid, &st, 0), cpid, "waitpid 子进程");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "子进程校验读到的数据");
}

int main(void)
{
    TEST_START("memfd_create / pidfd_*");

    signal(SIGPIPE, SIG_IGN);

    test_memfd_normal();
    test_memfd_empty_name();
    test_memfd_errors();
    test_memfd_name_limits();
    test_memfd_flags();
    test_memfd_seal_enforcement();
    test_memfd_hugetlb_and_reserved_flags();

    test_pidfd_open_self();
    test_pidfd_open_errors();

    test_pidfd_send_signal_paths();

    test_pidfd_getfd_flags();
    test_pidfd_getfd_cross_process();

    TEST_DONE();
}
