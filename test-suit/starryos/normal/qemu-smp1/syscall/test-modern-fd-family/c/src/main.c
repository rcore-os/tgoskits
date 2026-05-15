/*
 * test-modern-fd-family — memfd_create 与 pidfd_open / pidfd_send_signal / pidfd_getfd
 *
 * 对照 Linux man 2 语义；单可执行文件供 syscall grouped 流水线顺序执行。
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/sendfile.h>
#include <sys/syscall.h>
#include <sys/uio.h>
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

/* musl 可能无 fallocate/copy_file_range 封装，与 syscall 分组其它用例一致走 syscall */
static int x_fallocate(int fd, int mode, off_t offset, off_t len)
{
    return (int)syscall(SYS_fallocate, fd, mode, offset, len);
}

static ssize_t my_copy_file_range(int fd_in, off_t *off_in, int fd_out, off_t *off_out,
                                  size_t len, unsigned int flags)
{
    return syscall(SYS_copy_file_range, fd_in, off_in, fd_out, off_out, len, flags);
}

/* Unmapped user address for access_ok/EFAULT probes (independent of mmap_min_addr). */
static void *unmapped_user_addr(void)
{
    void *page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        return NULL;
    }
    (void)munmap(page, 4096);
    return page;
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

/* Linux: default memfd has F_SEAL_SEAL; F_ADD_SEALS(F_SEAL_WRITE) vs MAP_SHARED|PROT_WRITE -> EBUSY */
static void test_memfd_seal_abi_linux(void)
{
    printf("--- memfd seal Linux ABI (F_SEAL_SEAL default / EBUSY) ---\n");

    errno = 0;
    int fd = memfd_create("abi_default_seal", 0);
    CHECK(fd >= 0, "memfd_create(..., 0)");
    if (fd >= 0) {
        int seals = fcntl(fd, F_GET_SEALS);
        CHECK(seals >= 0 && (seals & F_SEAL_SEAL) == F_SEAL_SEAL,
              "无 MFD_ALLOW_SEALING: F_GET_SEALS 含 F_SEAL_SEAL");
        CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EPERM,
                  "无 MFD_ALLOW_SEALING: F_ADD_SEALS -> EPERM");
        close(fd);
    }

    errno = 0;
    fd = memfd_create("abi_busy_write", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(..., MFD_ALLOW_SEALING)");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");
        errno = 0;
        void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK(p != MAP_FAILED, "MAP_SHARED|PROT_WRITE mmap");
        if (p != MAP_FAILED) {
            CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY,
                      "已有 shared 可写映射: ADD_SEALS(F_SEAL_WRITE) -> EBUSY");
            CHECK_RET(munmap(p, 4096), 0, "munmap");
        }
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0,
                  "unmap 后 ADD_SEALS(F_SEAL_WRITE) 成功");
        close(fd);
    }
}

/* O(1) busy counter must observe mprotect upgrading MAP_SHARED to writable. */
static void test_memfd_seal_write_busy_after_mprotect(void)
{
    printf("--- memfd F_SEAL_WRITE busy after mprotect adds WRITE (MAP_SHARED) ---\n");

    errno = 0;
    int fd = memfd_create("abi_mprotect_seal", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(..., MFD_ALLOW_SEALING)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");

    errno = 0;
    void *p = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "MAP_SHARED|PROT_READ mmap");
    if (p == MAP_FAILED) {
        close(fd);
        return;
    }

    errno = 0;
    CHECK_RET(mprotect(p, 4096, PROT_READ | PROT_WRITE), 0, "mprotect add PROT_WRITE");

    errno = 0;
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY,
              "mprotect 后 shared 可写: ADD_SEALS(F_SEAL_WRITE) -> EBUSY");

    CHECK_RET(munmap(p, 4096), 0, "munmap");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "unmap 后 ADD_SEALS(F_SEAL_WRITE) 成功");
    close(fd);
}

/* Same inode, two fds: counter is per-memfd, not per-fd. */
static void test_memfd_seal_dup_two_fds_two_maps(void)
{
    printf("--- memfd two fds same inode: both MAP_SHARED|PROT_WRITE -> EBUSY ---\n");

    errno = 0;
    int fd = memfd_create("dup_seal_base", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 8192), 0, "ftruncate 8KiB");

    errno = 0;
    int fd2 = dup(fd);
    CHECK(fd2 >= 0, "dup(memfd)");
    if (fd2 < 0) {
        close(fd);
        return;
    }

    void *p1 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p1 != MAP_FAILED, "mmap fd1 page0");
    void *p2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd2, 4096);
    CHECK(p2 != MAP_FAILED, "mmap fd2 page1");
    if (p1 == MAP_FAILED || p2 == MAP_FAILED) {
        if (p1 != MAP_FAILED) {
            munmap(p1, 4096);
        }
        if (p2 != MAP_FAILED) {
            munmap(p2, 4096);
        }
        close(fd2);
        close(fd);
        return;
    }

    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY, "两 shared 可写映射 -> EBUSY");

    CHECK_RET(munmap(p1, 4096), 0, "munmap p1");
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY, "仍有一处映射 -> EBUSY");

    CHECK_RET(munmap(p2, 4096), 0, "munmap p2");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "全 unmap 后 seal 成功");

    close(fd2);
    close(fd);
}

/* Interior mprotect: middle pages READ-only, tails stay MAP_SHARED|PROT_WRITE (VMA split). */
static void test_memfd_seal_busy_after_mprotect_middle_read(void)
{
    printf("--- memfd: interior mprotect READ, tails still WRITE -> F_SEAL_WRITE EBUSY ---\n");

    long ps = sysconf(_SC_PAGESIZE);
    if (ps <= 0 || ps > 256 * 1024) {
        ps = 4096;
    }
    size_t maplen = (size_t)ps * 3u;

    errno = 0;
    int fd = memfd_create("mprotect_middle_seal", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(..., MFD_ALLOW_SEALING)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, (off_t)maplen), 0, "ftruncate 3 pages");

    errno = 0;
    void *p = mmap(NULL, maplen, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "MAP_SHARED|PROT_WRITE mmap 3 pages");
    if (p == MAP_FAILED) {
        close(fd);
        return;
    }

    unsigned char *base = (unsigned char *)p;
    errno = 0;
    CHECK_RET(mprotect(base + (size_t)ps, (size_t)ps, PROT_READ), 0,
              "mprotect interior page READ-only");

    errno = 0;
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY,
              "左右两VMA仍可写: ADD_SEALS(F_SEAL_WRITE) -> EBUSY");

    CHECK_RET(munmap(p, maplen), 0, "munmap 3 pages");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "全 unmap 后 seal 成功");
    close(fd);
}

/* Extra seal matrix: read-only shared vs F_SEAL_WRITE, F_SEAL_SEAL lock, grow|shrink combo, flags. */
static int mfd_get_seals(int fd)
{
    return fcntl(fd, F_GET_SEALS);
}

static void test_memfd_seal_extensions(void)
{
    printf("--- memfd seal extensions (read-only shared / SEAL lock / flags / proc fd) ---\n");

    /* MAP_SHARED|PROT_READ alone does not block F_SEAL_WRITE (no writable shared VMA). */
    errno = 0;
    int fd = memfd_create("seal_ro_shared", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(ro_shared)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");
    errno = 0;
    void *p_ro = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    CHECK(p_ro != MAP_FAILED, "MAP_SHARED|PROT_READ mmap");
    if (p_ro == MAP_FAILED) {
        close(fd);
        return;
    }
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "只读 shared 映射: ADD_SEALS(F_SEAL_WRITE) 成功");
    int seals = mfd_get_seals(fd);
    CHECK(seals >= 0 && (seals & F_SEAL_WRITE) != 0, "F_GET_SEALS 含 F_SEAL_WRITE");
    /* 与「未 seal 时仅 RO shared 不挡」对比：seal 后不能再新开 MAP_SHARED|PROT_WRITE（仍有 RO 映射时）。 */
    errno = 0;
    void *p_new = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p_new == MAP_FAILED && errno == EPERM,
          "F_SEAL_WRITE 后: 新开 MAP_SHARED|PROT_WRITE mmap -> EPERM (对比 RO shared 不挡 busy)");
    errno = 0;
    CHECK_ERR(mprotect(p_ro, 4096, PROT_READ | PROT_WRITE), EPERM,
              "F_SEAL_WRITE 后 mprotect 升级为可写 -> EPERM");
    CHECK_RET(munmap(p_ro, 4096), 0, "munmap ro shared");
    /* unmap 全部 shared 后仍禁止新的 shared 可写映射（seal 在 inode 上）。 */
    errno = 0;
    void *p_after = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p_after == MAP_FAILED && errno == EPERM,
          "F_SEAL_WRITE 且无 shared 映射后: 新 mmap MAP_SHARED|PROT_WRITE 仍 -> EPERM");
    CHECK_RET(close(fd), 0, "close memfd (ro shared case)");

    /* F_SEAL_SEAL: further ADD_SEALS fails; mask unchanged. */
    errno = 0;
    fd = memfd_create("seal_lock", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_lock)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_SEAL), 0, "ADD_SEALS(F_SEAL_SEAL)");
    seals = mfd_get_seals(fd);
    CHECK(seals >= 0 && (seals & F_SEAL_SEAL) != 0, "F_GET_SEALS 含 F_SEAL_SEAL");
    errno = 0;
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW), EPERM, "F_SEAL_SEAL 后 ADD_SEALS(F_SEAL_GROW) -> EPERM");
    CHECK(mfd_get_seals(fd) == seals, "F_SEAL_SEAL 失败后 seals 掩码不变");
    errno = 0;
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, 0), EPERM, "F_SEAL_SEAL 后 ADD_SEALS(0) -> EPERM");
    CHECK(mfd_get_seals(fd) == seals, "ADD_SEALS(0) 失败后掩码不变");
    close(fd);

    /* F_ADD_SEALS(0) is a no-op only before F_SEAL_SEAL (Linux-compatible). */
    errno = 0;
    int fd0 = memfd_create("add_zero", MFD_ALLOW_SEALING);
    CHECK(fd0 >= 0, "memfd_create(add_zero)");
    if (fd0 >= 0) {
        int z0 = mfd_get_seals(fd0);
        CHECK_RET(fcntl(fd0, F_ADD_SEALS, 0), 0, "未 F_SEAL_SEAL: F_ADD_SEALS(0) 成功");
        CHECK(mfd_get_seals(fd0) == z0, "ADD_SEALS(0) 后掩码不变");
        CHECK_RET(close(fd0), 0, "close add_zero");
    }

    /* F_SEAL_GROW | F_SEAL_SHRINK: grow and shrink both blocked; same size ok. */
    errno = 0;
    fd = memfd_create("seal_grow_shrink", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(grow_shrink)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW | F_SEAL_SHRINK), 0,
              "ADD_SEALS(F_SEAL_GROW|F_SEAL_SHRINK)");
    CHECK_ERR(ftruncate(fd, 8192), EPERM, "双 seal 后 grow -> EPERM");
    CHECK_ERR(ftruncate(fd, 2048), EPERM, "双 seal 后 shrink -> EPERM");
    CHECK_RET(ftruncate(fd, 4096), 0, "双 seal 后同尺寸 ftruncate 成功");
    close(fd);

    /* MAP_PRIVATE|PROT_WRITE + MAP_SHARED|PROT_READ: private does not block F_SEAL_WRITE. */
    errno = 0;
    fd = memfd_create("seal_mixed_maps", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(mixed_maps)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 12288), 0, "ftruncate 12KiB");
    void *p_sh = mmap(NULL, 4096, PROT_READ, MAP_SHARED, fd, 0);
    void *p_pr = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 4096);
    CHECK(p_sh != MAP_FAILED && p_pr != MAP_FAILED, "shared RO + private RW mmap");
    if (p_sh == MAP_FAILED || p_pr == MAP_FAILED) {
        if (p_sh != MAP_FAILED) {
            munmap(p_sh, 4096);
        }
        if (p_pr != MAP_FAILED) {
            munmap(p_pr, 4096);
        }
        close(fd);
        return;
    }
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "仅有 shared RO 时 ADD_SEALS(F_SEAL_WRITE) 成功");
    CHECK_RET(munmap(p_sh, 4096), 0, "munmap shared");
    CHECK_RET(munmap(p_pr, 4096), 0, "munmap private");
    close(fd);

    /* memfd_create: unknown flag bit 0x0100 -> EINVAL. */
    errno = 0;
    CHECK_ERR(memfd_create("bad_flag_0x100", 0x100u), EINVAL, "flags 0x0100 -> EINVAL");

    /* Re-open same inode via /proc/self/fd/N (new file description); seals + busy still apply. */
    errno = 0;
    fd = memfd_create("proc_reopen_seal", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(proc_reopen)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 8192), 0, "ftruncate 8KiB");
    char proc_path[64];
    (void)snprintf(proc_path, sizeof proc_path, "/proc/self/fd/%d", fd);
    errno = 0;
    int fd_re = open(proc_path, O_RDWR);
    if (fd_re < 0 && errno == ENOENT) {
        printf("  (skip: %s not available)\n", proc_path);
        close(fd);
        return;
    }
    CHECK(fd_re >= 0, "open /proc/self/fd/N 成功");
    if (fd_re < 0) {
        close(fd);
        return;
    }
    void *a = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    void *b = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd_re, 4096);
    CHECK(a != MAP_FAILED && b != MAP_FAILED, "proc-reopened fd mmap shared writable");
    if (a == MAP_FAILED || b == MAP_FAILED) {
        if (a != MAP_FAILED) {
            munmap(a, 4096);
        }
        if (b != MAP_FAILED) {
            munmap(b, 4096);
        }
        close(fd_re);
        close(fd);
        return;
    }
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY, "proc fd 第二映射: F_SEAL_WRITE -> EBUSY");
    CHECK_RET(munmap(a, 4096), 0, "munmap a");
    CHECK_ERR(fcntl(fd_re, F_ADD_SEALS, F_SEAL_WRITE), EBUSY, "仍有一处映射: EBUSY");
    CHECK_RET(munmap(b, 4096), 0, "munmap b");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "全 unmap 后 seal 成功");
    close(fd_re);
    close(fd);
}

/* fork: child inherits fd; F_SEAL_SEAL on inode blocks child's F_ADD_SEALS. */
static void test_memfd_seal_fork(void)
{
    printf("--- memfd seal: fork 子进程继承 fd，父已 F_SEAL_SEAL 则子不能再 ADD_SEALS ---\n");

    errno = 0;
    int fd = memfd_create("fork_seal_inherit", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(fork_seal_inherit)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "父: ADD_SEALS(F_SEAL_WRITE)");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_SEAL), 0, "父: ADD_SEALS(F_SEAL_SEAL)");

    pid_t cpid = fork();
    if (cpid < 0) {
        perror("fork");
        CHECK(0, "fork 失败");
        close(fd);
        return;
    }
    if (cpid == 0) {
        errno = 0;
        long r = (long)fcntl(fd, F_ADD_SEALS, F_SEAL_GROW);
        if (r == -1L && errno == EPERM) {
            _exit(0);
        }
        _exit(21);
    }

    int st = 0;
    CHECK_RET(waitpid(cpid, &st, 0), cpid, "waitpid fork 子进程");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0,
          "子进程: F_SEAL_SEAL 后 F_ADD_SEALS(F_SEAL_GROW) -> EPERM");

    CHECK_RET(close(fd), 0, "close memfd (fork case)");
}

/* fork clones VMAs: shared-writable memfd count must include child; EBUSY until parent munmap. */
static void test_memfd_seal_fork_busy_write_seal(void)
{
    printf("--- memfd: fork 复制共享可写 VMA -> F_SEAL_WRITE EBUSY 直至父 munmap ---\n");

    errno = 0;
    int fd = memfd_create("fork_busy_seal", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(fork_busy_seal)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");

    errno = 0;
    void *p = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(p != MAP_FAILED, "mmap MAP_SHARED|PROT_WRITE");
    if (p == MAP_FAILED) {
        close(fd);
        return;
    }

    pid_t cpid = fork();
    if (cpid < 0) {
        perror("fork");
        CHECK(0, "fork 失败");
        munmap(p, 4096);
        close(fd);
        return;
    }
    if (cpid == 0) {
        _exit(0);
    }

    errno = 0;
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY,
              "fork 后父子各一共享可写 VMA: ADD_SEALS(F_SEAL_WRITE) -> EBUSY");

    int st = 0;
    CHECK_RET(waitpid(cpid, &st, 0), cpid, "waitpid 子进程");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "子进程 _exit(0)");

    errno = 0;
    CHECK_ERR(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), EBUSY,
              "子进程退出后父映射仍在: ADD_SEALS(F_SEAL_WRITE) 仍 EBUSY");

    CHECK_RET(munmap(p, 4096), 0, "munmap 父进程映射");

    errno = 0;
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "父 munmap 后 ADD_SEALS(F_SEAL_WRITE) 成功");
    CHECK_RET(close(fd), 0, "close memfd (fork busy case)");
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
        /* 小文件 + 越 EOF 一字节：显式覆盖「隐式增长」路径 */
        CHECK_RET(ftruncate(fd, 100), 0, "ftruncate 100 用于 pwrite 越界");
        errno = 0;
        pw = pwrite(fd, "a", 1, 100);
        CHECK(pw == -1 && errno == EPERM, "F_SEAL_GROW: pwrite 于 EOF 外隐式扩展 -> EPERM");
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

/* Linux: F_SEAL_WRITE blocks non-zero writes; 0-byte write / pwrite succeed. */
static void test_memfd_sealed_zero_byte_write(void)
{
    printf("--- memfd F_SEAL_WRITE: 0-byte write / pwrite succeed (Linux) ---\n");

    errno = 0;
    int fd = memfd_create("seal_zero_len", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_zero_len)");
    if (fd < 0) {
        return;
    }
    CHECK_RET(ftruncate(fd, 4096), 0, "ftruncate 4KiB");
    CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");

    errno = 0;
    ssize_t z = write(fd, "", 0);
    CHECK(z == 0, "F_SEAL_WRITE 后 write(..., 0) 成功");

    errno = 0;
    z = pwrite(fd, "", 0, 0);
    CHECK(z == 0, "F_SEAL_WRITE 后 pwrite(..., 0) 成功");

    errno = 0;
    z = write(fd, "x", 1);
    CHECK(z == -1 && errno == EPERM, "F_SEAL_WRITE 后非零 write 仍 -> EPERM");

    CHECK_RET(close(fd), 0, "close memfd (zero-byte seal case)");
}

#define CLONE_VM_TEST_STACK (64u * 1024u)

/* Shared page layout: [0..3] ready (int), [4..7] done pipe write fd (int), [8] probe byte. */
static int clone_vm_child_fn(void *arg)
{
    unsigned char *page = (unsigned char *)arg;
    volatile int *ready = (volatile int *)page;
    int done_wr = ((int *)page)[1];

    while (*ready == 0) {
        (void)sched_yield();
    }
    page[8] = (unsigned char)0x5a;
    char b = 1;
    if (write(done_wr, &b, 1) != 1) {
        _exit(41);
    }
    _exit(0);
}

/* fork 出中间进程：其 `vm_aspace_shared` 为 false，再 `clone(CLONE_VM)` 子进程后先退出。 */
static void test_clone_vm_mid_exits_first(void)
{
    printf("--- CLONE_VM: intermediate exits first; child still uses shared mmap ---\n");

    int done_pipe[2];
    CHECK_RET(pipe(done_pipe), 0, "pipe (clone child -> parent done)");

    pid_t mid = fork();
    if (mid < 0) {
        perror("fork");
        CHECK(0, "fork intermediate");
        close(done_pipe[0]);
        close(done_pipe[1]);
        return;
    }
    if (mid == 0) {
        close(done_pipe[0]);
        void *stk = mmap(NULL, CLONE_VM_TEST_STACK, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (stk == MAP_FAILED) {
            _exit(10);
        }
        void *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        if (shared == MAP_FAILED) {
            munmap(stk, CLONE_VM_TEST_STACK);
            _exit(11);
        }
        memset(shared, 0, 4096);
        int *hdr = (int *)shared;
        hdr[0] = 0;
        hdr[1] = done_pipe[1];

        long clone_flags = (long)(CLONE_VM | SIGCHLD);
        pid_t cpid = clone(clone_vm_child_fn, (char *)stk + CLONE_VM_TEST_STACK, clone_flags, shared);
        if (cpid < 0) {
            munmap(shared, 4096);
            munmap(stk, CLONE_VM_TEST_STACK);
            _exit(12);
        }
        (void)cpid;
        hdr[0] = 1;
        /* Do not munmap `stk` / `shared`: clone child still runs in this address space. */
        _exit(0);
    }

    close(done_pipe[1]);

    int st = 0;
    CHECK_RET(waitpid(mid, &st, 0), mid, "waitpid intermediate");
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "intermediate _exit(0)");

    char ack = 0;
    ssize_t nr = read(done_pipe[0], &ack, 1);
    close(done_pipe[0]);
    CHECK(nr == 1 && ack == 1, "CLONE_VM child completed (pipe ack)");
}

static void test_memfd_seal_more_syscalls(void)
{
    printf("--- memfd seals: writev/pwritev/fallocate/sendfile/copy_file_range ---\n");

    ssize_t w;
    char one_byte = 'x';
    struct iovec iov;
    iov.iov_base = &one_byte;
    iov.iov_len = 1;

    /* F_SEAL_WRITE + writev */
    errno = 0;
    int fd = memfd_create("seal_writev", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_writev)");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "writev: ftruncate 4KiB");
        CHECK_RET(lseek(fd, 0, SEEK_SET), 0, "writev: lseek 0");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "writev: ADD_SEALS(F_SEAL_WRITE)");
        errno = 0;
        w = writev(fd, &iov, 1);
        CHECK(w == -1 && errno == EPERM, "F_SEAL_WRITE 后 writev -> EPERM");
        close(fd);
    }

    /* F_SEAL_WRITE + pwritev */
    errno = 0;
    fd = memfd_create("seal_pwritev", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_pwritev)");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "pwritev: ftruncate 4KiB");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "pwritev: ADD_SEALS(F_SEAL_WRITE)");
        errno = 0;
        w = pwritev(fd, &iov, 1, 0);
        CHECK(w == -1 && errno == EPERM, "F_SEAL_WRITE 后 pwritev -> EPERM");
        close(fd);
    }

    /* F_SEAL_WRITE + fallocate (grow file) */
    errno = 0;
    fd = memfd_create("seal_falloc_w", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_falloc_w)");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "falloc+WRITE: ftruncate 4KiB");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "falloc+WRITE: ADD_SEALS");
        CHECK_ERR(x_fallocate(fd, 0, 0, 8192), EPERM,
                  "F_SEAL_WRITE 后 fallocate 扩到 8KiB -> EPERM");
        close(fd);
    }

    /* F_SEAL_GROW + fallocate (grow file) */
    errno = 0;
    fd = memfd_create("seal_falloc_g", MFD_ALLOW_SEALING);
    CHECK(fd >= 0, "memfd_create(seal_falloc_g)");
    if (fd >= 0) {
        CHECK_RET(ftruncate(fd, 4096), 0, "falloc+GROW: ftruncate 4KiB");
        CHECK_RET(fcntl(fd, F_ADD_SEALS, F_SEAL_GROW), 0, "falloc+GROW: ADD_SEALS(F_SEAL_GROW)");
        CHECK_ERR(x_fallocate(fd, 0, 0, 8192), EPERM,
                  "F_SEAL_GROW 后 fallocate 扩到 8KiB -> EPERM");
        close(fd);
    }

    /*
     * ABI: sendfile EPERM must not advance the user-visible source offset (*off_in).
     * Keep these assertions when refactoring I/O ordering.
     */
    /* F_SEAL_WRITE + sendfile (out_fd sealed) */
    errno = 0;
    int out_fd = memfd_create("seal_sf_out", MFD_ALLOW_SEALING);
    int in_fd = memfd_create("seal_sf_in", 0);
    CHECK(out_fd >= 0 && in_fd >= 0, "memfd pair for sendfile");
    if (out_fd >= 0 && in_fd >= 0) {
        CHECK_RET(ftruncate(out_fd, 4096), 0, "sendfile: out ftruncate 4KiB");
        CHECK_RET(ftruncate(in_fd, 4096), 0, "sendfile: in ftruncate 4KiB");
        CHECK_RET(write(in_fd, "Z", 1), 1, "sendfile: in write 1 byte");
        CHECK_RET(lseek(in_fd, 0, SEEK_SET), 0, "sendfile: in lseek 0");
        CHECK_RET(fcntl(out_fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "sendfile: out ADD_SEALS");
        off_t off_in = 0;
        errno = 0;
        ssize_t sf = sendfile(out_fd, in_fd, &off_in, 1);
        CHECK(sf == -1 && errno == EPERM, "sendfile 写入 sealed memfd -> EPERM");
        CHECK(off_in == 0, "sendfile EPERM: 用户可见 in_fd offset 不变");
        close(out_fd);
        close(in_fd);
    } else {
        if (out_fd >= 0) {
            close(out_fd);
        }
        if (in_fd >= 0) {
            close(in_fd);
        }
    }

    /*
     * ABI: copy_file_range EPERM must not advance user-visible off_in/off_out.
     * Keep these assertions when refactoring I/O ordering.
     */
    /* F_SEAL_WRITE + copy_file_range (into sealed memfd) */
    errno = 0;
    int src_fd = memfd_create("seal_cfr_in", 0);
    int dst_fd = memfd_create("seal_cfr_out", MFD_ALLOW_SEALING);
    CHECK(src_fd >= 0 && dst_fd >= 0, "memfd pair for copy_file_range");
    if (src_fd >= 0 && dst_fd >= 0) {
        CHECK_RET(ftruncate(src_fd, 4096), 0, "cfr: src ftruncate 4KiB");
        CHECK_RET(write(src_fd, "Q", 1), 1, "cfr: src write");
        CHECK_RET(lseek(src_fd, 0, SEEK_SET), 0, "cfr: src lseek 0");
        CHECK_RET(ftruncate(dst_fd, 4096), 0, "cfr: dst ftruncate 4KiB");
        CHECK_RET(fcntl(dst_fd, F_ADD_SEALS, F_SEAL_WRITE), 0, "cfr: dst ADD_SEALS");
        off_t off_in = 0;
        off_t off_out = 0;
        errno = 0;
        ssize_t cfr = my_copy_file_range(src_fd, &off_in, dst_fd, &off_out, 1, 0);
        CHECK(cfr == -1 && errno == EPERM, "copy_file_range 写入 sealed memfd -> EPERM");
        CHECK(off_in == 0 && off_out == 0,
              "copy_file_range EPERM: 用户可见 off_in/off_out 不变");
        close(src_fd);
        close(dst_fd);
    } else {
        if (src_fd >= 0) {
            close(src_fd);
        }
        if (dst_fd >= 0) {
            close(dst_fd);
        }
    }
}

/*
 * Sealed memfd / MAP_FIXED ordering: EFAULT vs EPERM, EACCES before destructive unmap,
 * sendfile/copy_file_range user offsets unchanged on failure.
 */
static void test_memfd_seal_syscall_ordering_regressions(void)
{
    printf("--- memfd seal: EFAULT/EACCES/EPERM ordering + MAP_FIXED no teardown + offsets ---\n");

    void *bad_addr = unmapped_user_addr();

    /* writev: bad iovec buffer on sealed memfd -> EFAULT (access_ok before seal denial). */
    errno = 0;
    int sealed = memfd_create("seal_order_writev", MFD_ALLOW_SEALING);
    CHECK(sealed >= 0, "memfd_create(seal_order_writev)");
    if (sealed >= 0) {
        CHECK_RET(ftruncate(sealed, 4096), 0, "ftruncate");
        CHECK_RET(fcntl(sealed, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");
        struct iovec bad_iov;
        bad_iov.iov_base = bad_addr;
        bad_iov.iov_len = 1;
        errno = 0;
        ssize_t w = writev(sealed, &bad_iov, 1);
        CHECK(w == -1 && errno == EFAULT, "sealed memfd + 坏 iov -> EFAULT");
        CHECK_RET(close(sealed), 0, "close sealed memfd (writev case)");
    }

    /* write: bad user buffer on sealed memfd -> EFAULT. */
    errno = 0;
    sealed = memfd_create("seal_order_write", MFD_ALLOW_SEALING);
    CHECK(sealed >= 0, "memfd_create(seal_order_write)");
    if (sealed >= 0) {
        CHECK_RET(ftruncate(sealed, 4096), 0, "ftruncate");
        CHECK_RET(fcntl(sealed, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");
        errno = 0;
        ssize_t ww = write(sealed, bad_addr, 1);
        CHECK(ww == -1 && errno == EFAULT, "sealed memfd + 坏 buf -> EFAULT");
        CHECK_RET(close(sealed), 0, "close sealed memfd (write case)");
    }

    /* Valid user buffer on sealed memfd -> EPERM; file content and offset unchanged. */
    errno = 0;
    sealed = memfd_create("seal_order_valid", MFD_ALLOW_SEALING);
    CHECK(sealed >= 0, "memfd_create(seal_order_valid)");
    if (sealed >= 0) {
        const char seed = 'A';
        const char payload = 'B';
        CHECK_RET(ftruncate(sealed, 4096), 0, "ftruncate");
        CHECK_RET(write(sealed, &seed, 1), 1, "write seed byte");
        CHECK_RET(lseek(sealed, 0, SEEK_SET), 0, "lseek 0 after seed");
        CHECK_RET(fcntl(sealed, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");
        off_t pos_before = lseek(sealed, 0, SEEK_CUR);
        CHECK(pos_before == 0, "offset before sealed write is 0");
        errno = 0;
        ssize_t wv = write(sealed, &payload, 1);
        CHECK(wv == -1 && errno == EPERM, "sealed memfd + 合法 buf write -> EPERM");
        off_t pos_after = lseek(sealed, 0, SEEK_CUR);
        CHECK(pos_after == pos_before, "sealed write EPERM: file offset unchanged");
        CHECK_RET(lseek(sealed, 0, SEEK_SET), 0, "lseek 0 for readback");
        char got = 0;
        CHECK_RET(read(sealed, &got, 1), 1, "read back one byte");
        CHECK(got == seed, "sealed write EPERM: file content unchanged");
        struct iovec good_iov;
        good_iov.iov_base = (void *)&payload;
        good_iov.iov_len = 1;
        errno = 0;
        ssize_t wvi = writev(sealed, &good_iov, 1);
        CHECK(wvi == -1 && errno == EPERM, "sealed memfd + 合法 iov writev -> EPERM");
        CHECK_RET(lseek(sealed, 0, SEEK_SET), 0, "lseek 0 after writev");
        got = 0;
        CHECK_RET(read(sealed, &got, 1), 1, "read back after writev");
        CHECK(got == seed, "sealed writev EPERM: file content unchanged");
        CHECK_RET(close(sealed), 0, "close sealed memfd (valid buf case)");
    }

    /* MAP_FIXED + MAP_SHARED|PROT_WRITE on O_RDONLY file -> EACCES; prior anon map intact. */
    errno = 0;
    int ro = open("/proc/self/exe", O_RDONLY);
    CHECK(ro >= 0, "open /proc/self/exe O_RDONLY");
    if (ro >= 0) {
        void *slot = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(slot != MAP_FAILED, "anon mmap for MAP_FIXED probe");
        if (slot != MAP_FAILED) {
            *(volatile unsigned char *)slot = 0x5a;
            errno = 0;
            void *bad = mmap(slot, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, ro, 0);
            CHECK(bad == MAP_FAILED && errno == EACCES,
                  "MAP_FIXED shared-writable 只读 fd -> EACCES");
            CHECK(*(unsigned char *)slot == 0x5a, "MAP_FIXED EACCES 后旧映射未被破坏");
            CHECK_RET(munmap(slot, 4096), 0, "munmap probe");
        }
        CHECK_RET(close(ro), 0, "close ro exe");
    }

    /* MAP_FIXED + sealed memfd shared writable -> EPERM; prior anon map intact. */
    errno = 0;
    int mfd = memfd_create("seal_order_mmap", MFD_ALLOW_SEALING);
    CHECK(mfd >= 0, "memfd_create(seal_order_mmap)");
    if (mfd >= 0) {
        CHECK_RET(ftruncate(mfd, 4096), 0, "ftruncate memfd");
        CHECK_RET(fcntl(mfd, F_ADD_SEALS, F_SEAL_WRITE), 0, "ADD_SEALS(F_SEAL_WRITE)");
        void *slot2 = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(slot2 != MAP_FAILED, "anon mmap for MAP_FIXED seal probe");
        if (slot2 != MAP_FAILED) {
            *(volatile unsigned char *)slot2 = 0xa7;
            errno = 0;
            void *bad2 =
                mmap(slot2, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, mfd, 0);
            CHECK(bad2 == MAP_FAILED && errno == EPERM,
                  "MAP_FIXED shared-writable sealed memfd -> EPERM");
            CHECK(*(unsigned char *)slot2 == 0xa7, "MAP_FIXED EPERM 后旧映射未被破坏");
            CHECK_RET(munmap(slot2, 4096), 0, "munmap seal probe");
        }
        CHECK_RET(close(mfd), 0, "close sealed memfd (mmap case)");
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

/* Linux pidfd_open(2): EINVAL when pid is not valid; 0 is not a referrable pid. */
static void test_pidfd_open_pid_zero_linux(void)
{
    printf("--- pidfd_open pid==0 (Linux EINVAL) ---\n");

    errno = 0;
    CHECK_ERR(x_pidfd_open(0, 0), EINVAL, "pid==0 -> EINVAL");

#ifndef PIDFD_NONBLOCK
#define PIDFD_NONBLOCK 2048u
#endif
#ifndef PIDFD_THREAD
#define PIDFD_THREAD 128u
#endif
    errno = 0;
    CHECK_ERR(x_pidfd_open(0, PIDFD_NONBLOCK), EINVAL, "pid==0 + PIDFD_NONBLOCK -> EINVAL");
    errno = 0;
    CHECK_ERR(x_pidfd_open(0, PIDFD_THREAD), EINVAL, "pid==0 + PIDFD_THREAD -> EINVAL");
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
    test_memfd_seal_abi_linux();
    test_memfd_seal_write_busy_after_mprotect();
    test_memfd_seal_dup_two_fds_two_maps();
    test_memfd_seal_busy_after_mprotect_middle_read();
    test_memfd_seal_extensions();
    test_memfd_seal_fork();
    test_memfd_seal_fork_busy_write_seal();
    test_memfd_seal_enforcement();
    test_memfd_sealed_zero_byte_write();
    test_clone_vm_mid_exits_first();
    test_memfd_seal_more_syscalls();
    test_memfd_seal_syscall_ordering_regressions();
    test_memfd_hugetlb_and_reserved_flags();

    test_pidfd_open_self();
    test_pidfd_open_errors();
    test_pidfd_open_pid_zero_linux();

    test_pidfd_send_signal_paths();

    test_pidfd_getfd_flags();
    test_pidfd_getfd_cross_process();

    TEST_DONE();
}
