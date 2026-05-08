/*
 * test-stat-family
 *
 * 覆盖 stat/lstat/fstat/fstatat/statx/statfs/fstatfs 的正常路径与错误路径。
 * 参考 man 2 stat / statx / statfs 与 linux-compatible-testsuit/tests/test_stat.c。
 *
 * 注意事项：
 *  - riscv64/aarch64/loongarch64 没有独立的 stat/lstat syscall；musl 通过
 *    newfstatat(AT_FDCWD, ...) 封装，所以 stat()/lstat() 依旧可用。
 *  - 一些错误路径（如 EFAULT/非法 flag）必须绕过 libc 做参数净化，直接
 *    走 syscall()。
 */

#include "test_framework.h"

#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <stdint.h>

/*
 * statx 在部分 musl 头文件里没有 C wrapper，用 syscall() 兜底。
 */
#ifndef AT_STATX_SYNC_AS_STAT
#define AT_STATX_SYNC_AS_STAT 0x0000
#endif
#ifndef AT_STATX_FORCE_SYNC
#define AT_STATX_FORCE_SYNC 0x2000
#endif
#ifndef AT_STATX_DONT_SYNC
#define AT_STATX_DONT_SYNC 0x4000
#endif
#ifndef STATX_BASIC_STATS
#define STATX_BASIC_STATS 0x000007ffU
#endif
#ifndef STATX_TYPE
#define STATX_TYPE 0x0001U
#endif
#ifndef STATX_SIZE
#define STATX_SIZE 0x0200U
#endif
#ifndef STATX_MODE
#define STATX_MODE 0x0002U
#endif
#ifndef STATX_NLINK
#define STATX_NLINK 0x0004U
#endif
#ifndef STATX_UID
#define STATX_UID 0x0008U
#endif
#ifndef STATX_GID
#define STATX_GID 0x0010U
#endif
#ifndef STATX_INO
#define STATX_INO 0x0100U
#endif
#ifndef STATX_BLOCKS
#define STATX_BLOCKS 0x0400U
#endif
#ifndef STATX_ATIME
#define STATX_ATIME 0x0020U
#endif
#ifndef STATX_MTIME
#define STATX_MTIME 0x0040U
#endif
#ifndef STATX_CTIME
#define STATX_CTIME 0x0080U
#endif

struct statx_ts {
    int64_t  tv_sec;
    uint32_t tv_nsec;
    int32_t  __reserved;
};
struct statx_buf {
    uint32_t stx_mask;
    uint32_t stx_blksize;
    uint64_t stx_attributes;
    uint32_t stx_nlink;
    uint32_t stx_uid;
    uint32_t stx_gid;
    uint16_t stx_mode;
    uint16_t __pad0;
    uint64_t stx_ino;
    uint64_t stx_size;
    uint64_t stx_blocks;
    uint64_t stx_attributes_mask;
    struct statx_ts stx_atime;
    struct statx_ts stx_btime;
    struct statx_ts stx_ctime;
    struct statx_ts stx_mtime;
    uint32_t stx_rdev_major;
    uint32_t stx_rdev_minor;
    uint32_t stx_dev_major;
    uint32_t stx_dev_minor;
    uint64_t __spare[14];
};

static long raw_statx(int dirfd, const char *path, unsigned flags,
                      unsigned mask, struct statx_buf *buf)
{
    return syscall(SYS_statx, dirfd, path, flags, mask, buf);
}


/* ---------------------------------------------------------------- */

static const char *BASE = "/tmp/starry_stat_test";
static char REG[128];      /* regular file */
static char SUBDIR[128];   /* subdirectory */
static char LINK[128];     /* symlink -> REG */
static char DANG[128];     /* dangling symlink */
static char LOOPA[128];    /* loop_a -> loop_b */
static char LOOPB[128];    /* loop_b -> loop_a */

static void setup(void)
{
    /* Start fresh */
    char cmd[256];
    snprintf(cmd, sizeof(cmd), "rm -rf %s", BASE);
    system(cmd);

    mkdir(BASE, 0755);

    snprintf(REG,    sizeof(REG),    "%s/regfile.txt",    BASE);
    snprintf(SUBDIR, sizeof(SUBDIR), "%s/subdir",         BASE);
    snprintf(LINK,   sizeof(LINK),   "%s/link_to_reg",    BASE);
    snprintf(DANG,   sizeof(DANG),   "%s/dangling_link",  BASE);
    snprintf(LOOPA,  sizeof(LOOPA),  "%s/loop_a",         BASE);
    snprintf(LOOPB,  sizeof(LOOPB),  "%s/loop_b",         BASE);

    int fd = open(REG, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) {
        const char *msg = "Hello, stat!";  /* 12 bytes */
        ssize_t _w = write(fd, msg, 12);
        (void)_w;
        close(fd);
    }
    mkdir(SUBDIR, 0755);
    symlink(REG, LINK);
    symlink("/tmp/no_such_file_for_starry_stat_test", DANG);
    symlink(LOOPB, LOOPA);
    symlink(LOOPA, LOOPB);
}

static void teardown(void)
{
    char cmd[256];
    snprintf(cmd, sizeof(cmd), "rm -rf %s", BASE);
    system(cmd);
}

/* ---------------------------------------------------------------- */

static void test_stat_regular_file(void)
{
    struct stat sb;
    CHECK_RET(stat(REG, &sb), 0, "stat 普通文件");
    CHECK(S_ISREG(sb.st_mode), "st_mode 类型是 regular");
    CHECK(sb.st_size == 12, "st_size == 12 (\"Hello, stat!\")");
    CHECK(sb.st_nlink >= 1, "st_nlink >= 1");
    CHECK(sb.st_blksize > 0, "st_blksize > 0");
    CHECK(sb.st_ino > 0, "st_ino > 0");
    /* 权限位由 umask 影响，只检查用户读写位 */
    CHECK((sb.st_mode & 0600) == 0600, "st_mode 含 owner rw");
}

static void test_stat_directory(void)
{
    struct stat sb;
    CHECK_RET(stat(SUBDIR, &sb), 0, "stat 目录");
    CHECK(S_ISDIR(sb.st_mode), "st_mode 类型是 directory");
    CHECK(sb.st_nlink >= 1, "目录 st_nlink >= 1");
}

static void test_stat_symlink_follows(void)
{
    struct stat sb_link, sb_reg;
    CHECK_RET(stat(LINK, &sb_link), 0, "stat symlink (跟随)");
    CHECK_RET(stat(REG,  &sb_reg),  0, "stat 目标文件");
    CHECK(S_ISREG(sb_link.st_mode), "stat(symlink) 报告 regular");
    CHECK(sb_link.st_ino == sb_reg.st_ino, "stat(symlink) 与目标同 inode");
}

static void test_lstat_symlink_no_follow(void)
{
    struct stat sb_link, sb_reg;
    CHECK_RET(lstat(LINK, &sb_link), 0, "lstat symlink (不跟随)");
    CHECK_RET(stat(REG,   &sb_reg),  0, "stat 目标文件");
    CHECK(S_ISLNK(sb_link.st_mode), "lstat(symlink) 报告 symlink");
    CHECK(sb_link.st_ino != sb_reg.st_ino, "lstat(symlink) 与目标不同 inode");
}

static void test_fstat_regular_file(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open 普通文件");
    struct stat sb;
    CHECK_RET(fstat(fd, &sb), 0, "fstat 普通文件");
    CHECK(S_ISREG(sb.st_mode), "fstat: regular");
    CHECK(sb.st_size == 12, "fstat: st_size == 12");
    close(fd);
}

static void test_fstat_directory(void)
{
    int fd = open(SUBDIR, O_RDONLY | O_DIRECTORY);
    CHECK(fd >= 0, "open 目录 O_DIRECTORY");
    struct stat sb;
    CHECK_RET(fstat(fd, &sb), 0, "fstat 目录");
    CHECK(S_ISDIR(sb.st_mode), "fstat: directory");
    close(fd);
}

static void test_stat_lstat_same_for_regular(void)
{
    struct stat s1, s2;
    CHECK_RET(stat(REG,  &s1), 0, "stat 普通文件 (比较)");
    CHECK_RET(lstat(REG, &s2), 0, "lstat 普通文件 (比较)");
    CHECK(s1.st_ino == s2.st_ino, "stat/lstat 普通文件 inode 一致");
    CHECK(s1.st_dev == s2.st_dev, "stat/lstat 普通文件 dev 一致");
}

/* ---------- 错误路径 ---------- */

static void test_enoent_missing(void)
{
    struct stat sb;
    char path[200];
    snprintf(path, sizeof(path), "%s/no_such_file_xyz", BASE);
    CHECK_ERR(stat(path, &sb), ENOENT, "stat 不存在 → ENOENT");
}

static void test_enoent_empty_path(void)
{
    struct stat sb;
    /* 空路径在无 AT_EMPTY_PATH 时应返回 ENOENT */
    CHECK_ERR(stat("", &sb), ENOENT, "stat(\"\") → ENOENT");
}

static void test_enoent_dangling_symlink(void)
{
    struct stat sb;
    /* stat 跟随 symlink 到不存在目标 → ENOENT */
    CHECK_ERR(stat(DANG, &sb), ENOENT, "stat 悬挂 symlink → ENOENT");

    /* 但 lstat 不跟随，应成功 */
    struct stat sb2;
    CHECK_RET(lstat(DANG, &sb2), 0, "lstat 悬挂 symlink 成功");
    CHECK(S_ISLNK(sb2.st_mode), "lstat 悬挂 symlink: S_ISLNK");
}

static void test_eloop(void)
{
    struct stat sb;
    CHECK_ERR(stat(LOOPA, &sb), ELOOP, "stat 循环 symlink → ELOOP");
}

static void test_enotdir(void)
{
    struct stat sb;
    char path[200];
    snprintf(path, sizeof(path), "%s/regfile.txt/child", BASE);
    CHECK_ERR(stat(path, &sb), ENOTDIR, "stat (普通文件)/child → ENOTDIR");
}

static void test_ebadf_fstat(void)
{
    struct stat sb;
    /* -1 一定是非法 fd */
    CHECK_ERR(fstat(-1, &sb), EBADF, "fstat(-1) → EBADF");
}

static void test_ebadf_closed(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open 普通文件");
    close(fd);
    struct stat sb;
    CHECK_ERR(fstat(fd, &sb), EBADF, "fstat(已关闭 fd) → EBADF");
}

static void test_efault_statbuf_null(void)
{
    /* libc 可能在 stat 前校验 NULL，直接走 SYS_statx（所有 4 个 arch 均可用） */
    errno = 0;
    long rc = syscall(SYS_statx, AT_FDCWD, REG, 0,
                      STATX_BASIC_STATS, (void *)0);
    CHECK(rc == -1 && errno == EFAULT, "statx(buf=NULL) → EFAULT");
}

/* fstatat(AT_FDCWD, path, &sb, 0xFFFF) 应返回 EINVAL */
static void test_fstatat_einval_unknown_flag(void)
{
    struct stat sb;
    CHECK_ERR(fstatat(AT_FDCWD, REG, &sb, 0xFFFF), EINVAL,
              "fstatat 未知 flag 全开 → EINVAL");
}

static void test_fstatat_symlink_nofollow(void)
{
    struct stat sb;
    CHECK_RET(fstatat(AT_FDCWD, LINK, &sb, AT_SYMLINK_NOFOLLOW), 0,
              "fstatat(AT_SYMLINK_NOFOLLOW) 成功");
    CHECK(S_ISLNK(sb.st_mode), "fstatat(symlink_nofollow) → S_ISLNK");
}

static void test_fstatat_at_empty_path(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open reg");
    struct stat sb;
    CHECK_RET(fstatat(fd, "", &sb, AT_EMPTY_PATH), 0,
              "fstatat(fd, \"\", AT_EMPTY_PATH) 成功");
    CHECK(S_ISREG(sb.st_mode), "fstatat(AT_EMPTY_PATH) → regular");
    CHECK(sb.st_size == 12, "fstatat(AT_EMPTY_PATH) → size 12");
    close(fd);
}

static void test_fstatat_empty_path_no_flag_enoent(void)
{
    struct stat sb;
    CHECK_ERR(fstatat(AT_FDCWD, "", &sb, 0), ENOENT,
              "fstatat(AT_FDCWD, \"\", 0) → ENOENT");
}

/* ---------- statx ---------- */

static void test_statx_regular(void)
{
    struct statx_buf stx;
    memset(&stx, 0, sizeof(stx));
    CHECK_RET(raw_statx(AT_FDCWD, REG, AT_STATX_SYNC_AS_STAT,
                        STATX_BASIC_STATS, &stx),
              0, "statx 普通文件");
    CHECK((stx.stx_mask & STATX_TYPE) != 0, "statx mask 含 TYPE");
    CHECK((stx.stx_mask & STATX_SIZE) != 0, "statx mask 含 SIZE");
    CHECK(S_ISREG(stx.stx_mode), "statx: regular");
    CHECK(stx.stx_size == 12, "statx: stx_size == 12");
}

static void test_statx_symlink_nofollow(void)
{
    struct statx_buf stx;
    memset(&stx, 0, sizeof(stx));
    CHECK_RET(raw_statx(AT_FDCWD, LINK, AT_SYMLINK_NOFOLLOW,
                        STATX_BASIC_STATS, &stx),
              0, "statx symlink AT_SYMLINK_NOFOLLOW");
    CHECK(S_ISLNK(stx.stx_mode), "statx(symlink_nofollow) → S_ISLNK");
}

static void test_statx_empty_path_with_flag(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open reg");
    struct statx_buf stx;
    memset(&stx, 0, sizeof(stx));
    CHECK_RET(raw_statx(fd, "", AT_EMPTY_PATH,
                        STATX_BASIC_STATS, &stx),
              0, "statx(fd, \"\", AT_EMPTY_PATH)");
    CHECK(S_ISREG(stx.stx_mode), "statx(AT_EMPTY_PATH) → regular");
    CHECK(stx.stx_size == 12, "statx(AT_EMPTY_PATH) → size 12");
    close(fd);
}

static void test_statx_empty_path_no_flag_enoent(void)
{
    struct statx_buf stx;
    errno = 0;
    long rc = raw_statx(AT_FDCWD, "", 0, STATX_BASIC_STATS, &stx);
    CHECK(rc == -1 && errno == ENOENT,
          "statx(AT_FDCWD, \"\", 0) → ENOENT");
}

static void test_statx_reserved_mask_einval(void)
{
    struct statx_buf stx;
    errno = 0;
    /* 0x80000000 是 STATX 保留位；Linux 返回 EINVAL。 */
    long rc = raw_statx(AT_FDCWD, REG, 0, 0x80000000U, &stx);
    CHECK(rc == -1 && errno == EINVAL,
          "statx 保留 mask 位 → EINVAL");
}

static void test_statx_sync_type_einval(void)
{
    struct statx_buf stx;
    errno = 0;
    /* FORCE_SYNC | DONT_SYNC 互斥；Linux 返回 EINVAL */
    long rc = raw_statx(AT_FDCWD, REG,
                        AT_STATX_FORCE_SYNC | AT_STATX_DONT_SYNC,
                        STATX_BASIC_STATS, &stx);
    CHECK(rc == -1 && errno == EINVAL,
          "statx FORCE_SYNC|DONT_SYNC 同设 → EINVAL");
}

/* ---------- 深层覆盖 ---------- */

/* fstatat 相对 dirfd 查找 */
static void test_fstatat_relative_to_dirfd(void)
{
    int dfd = open(BASE, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0, "open BASE as dirfd");

    struct stat sb;
    CHECK_RET(fstatat(dfd, "regfile.txt", &sb, 0), 0,
              "fstatat(dirfd, \"regfile.txt\") 相对解析");
    CHECK(S_ISREG(sb.st_mode), "fstatat(dirfd, regfile): regular");
    CHECK(sb.st_size == 12, "fstatat(dirfd, regfile): size 12");

    CHECK_ERR(fstatat(dfd, "no_such_child", &sb, 0), ENOENT,
              "fstatat(dirfd, 不存在) → ENOENT");

    close(dfd);
}

/* fstatat 绝对路径忽略 dirfd */
static void test_fstatat_abs_ignores_dirfd(void)
{
    int dfd = open(SUBDIR, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0, "open SUBDIR as dirfd (不含 REG)");
    struct stat sb;
    CHECK_RET(fstatat(dfd, REG, &sb, 0), 0,
              "fstatat 绝对路径忽略 dirfd");
    CHECK(sb.st_size == 12, "fstatat 绝对路径: size 12");
    close(dfd);
}

/* fstat 对 pipe fd 报告 S_ISFIFO */
static void test_fstat_pipe(void)
{
    int fds[2];
    CHECK(pipe(fds) == 0, "pipe()");
    struct stat sb;
    CHECK_RET(fstat(fds[0], &sb), 0, "fstat(pipe 读端)");
    CHECK(S_ISFIFO(sb.st_mode), "pipe 读端 → S_ISFIFO");
    CHECK_RET(fstat(fds[1], &sb), 0, "fstat(pipe 写端)");
    CHECK(S_ISFIFO(sb.st_mode), "pipe 写端 → S_ISFIFO");
    close(fds[0]);
    close(fds[1]);
}

/* stat /dev/null 应报告 S_ISCHR */
static void test_stat_chardev(void)
{
    struct stat sb;
    CHECK_RET(stat("/dev/null", &sb), 0, "stat /dev/null");
    CHECK(S_ISCHR(sb.st_mode), "/dev/null → S_ISCHR");
}

/* 目录 st_nlink 至少为 2（"."+"..") */
static void test_directory_nlink(void)
{
    struct stat sb;
    CHECK_RET(stat(SUBDIR, &sb), 0, "stat SUBDIR");
    CHECK(sb.st_nlink >= 2, "目录 nlink >= 2 (\".\" + \"..\")");
}

/* stat 在 ftruncate 之后反映新大小 */
static void test_stat_after_ftruncate(void)
{
    char path[200];
    snprintf(path, sizeof(path), "%s/trunc.txt", BASE);
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open trunc.txt O_CREAT");
    CHECK_RET(ftruncate(fd, 8192), 0, "ftruncate to 8192");

    struct stat sb;
    CHECK_RET(fstat(fd, &sb), 0, "fstat after ftruncate");
    CHECK(sb.st_size == 8192, "st_size 反映 ftruncate");

    close(fd);
    unlink(path);
}

/* chmod 后权限位可见 */
static void test_stat_after_chmod(void)
{
    struct stat sb;
    CHECK_RET(chmod(REG, 0600), 0, "chmod 0600");
    CHECK_RET(stat(REG, &sb), 0, "stat REG (chmod 后)");
    CHECK((sb.st_mode & 0777) == 0600, "mode & 0777 == 0600");
    CHECK_RET(chmod(REG, 0644), 0, "chmod 0644 复位");
}

/* statx 返回的 mask 应覆盖 BASIC_STATS 全部字段 */
static void test_statx_mask_contains_basic(void)
{
    struct statx_buf stx;
    memset(&stx, 0, sizeof(stx));
    CHECK_RET(raw_statx(AT_FDCWD, REG, 0, STATX_BASIC_STATS, &stx), 0,
              "statx 请求 BASIC_STATS");
    unsigned need = STATX_TYPE | STATX_MODE | STATX_NLINK |
                    STATX_UID | STATX_GID | STATX_INO | STATX_SIZE |
                    STATX_BLOCKS | STATX_ATIME | STATX_MTIME | STATX_CTIME;
    CHECK((stx.stx_mask & need) == need,
          "statx stx_mask 覆盖 BASIC_STATS 全部位");
}

/* statx 在字符设备上正确填充 rdev */
static void test_statx_chardev_rdev(void)
{
    struct statx_buf stx;
    memset(&stx, 0, sizeof(stx));
    CHECK_RET(raw_statx(AT_FDCWD, "/dev/null", 0, STATX_BASIC_STATS, &stx), 0,
              "statx /dev/null");
    CHECK(S_ISCHR(stx.stx_mode), "statx /dev/null → S_ISCHR");
    CHECK(stx.stx_rdev_major != 0 || stx.stx_rdev_minor != 0,
          "statx /dev/null rdev 非全零");
}

/* statx 未知 flag → EINVAL */
static void test_statx_unknown_flag_einval(void)
{
    struct statx_buf stx;
    errno = 0;
    /* 0x100000 超出 AT_* / AT_STATX_* 允许范围 */
    long rc = raw_statx(AT_FDCWD, REG, 0x100000u, STATX_BASIC_STATS, &stx);
    CHECK(rc == -1 && errno == EINVAL, "statx 未知 flag → EINVAL");
}

/* 超大 fd fstat → EBADF */
static void test_fstat_huge_fd_ebadf(void)
{
    struct stat sb;
    CHECK_ERR(fstat(99999, &sb), EBADF, "fstat(99999) → EBADF");
}

/* dirfd 是常规文件 fd + 非空 path → ENOTDIR */
static void test_fstatat_dirfd_is_file(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open REG");
    struct stat sb;
    CHECK_ERR(fstatat(fd, "child", &sb, 0), ENOTDIR,
              "fstatat(reg fd, 相对路径) → ENOTDIR");
    close(fd);
}

/* stat/fstat 对同一文件返回相同 dev + ino */
static void test_stat_fstat_same_dev_ino(void)
{
    struct stat a, b;
    CHECK_RET(stat(REG, &a), 0, "stat REG");
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open REG");
    CHECK_RET(fstat(fd, &b), 0, "fstat fd REG");
    CHECK(a.st_ino == b.st_ino, "stat/fstat 同文件 inode 一致");
    CHECK(a.st_dev == b.st_dev, "stat/fstat 同文件 dev 一致");
    close(fd);
}

/* 硬链接：nlink 应该从 1 → 2，两个路径 inode 相同 */
static void test_stat_hardlink_nlink(void)
{
    char src[200], dst[200];
    snprintf(src, sizeof(src), "%s/hl_src", BASE);
    snprintf(dst, sizeof(dst), "%s/hl_dst", BASE);

    int fd = open(src, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "open hl_src");
    ssize_t _w = write(fd, "abcd", 4);
    (void)_w;
    close(fd);

    CHECK_RET(link(src, dst), 0, "link(src, dst)");

    struct stat a, b;
    CHECK_RET(stat(src, &a), 0, "stat src");
    CHECK_RET(stat(dst, &b), 0, "stat dst");
    CHECK(a.st_ino == b.st_ino, "硬链接 inode 一致");
    CHECK(a.st_nlink == 2, "硬链接后 src nlink == 2");
    CHECK(b.st_nlink == 2, "硬链接后 dst nlink == 2");

    unlink(dst);
    CHECK_RET(stat(src, &a), 0, "stat src (unlink dst 后)");
    CHECK(a.st_nlink == 1, "unlink dst 后 src nlink == 1");

    unlink(src);
}

/* write 扩展文件后 fstat st_size 应更新 */
static void test_fstat_size_after_write(void)
{
    char path[200];
    snprintf(path, sizeof(path), "%s/w_size.txt", BASE);
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "open w_size.txt");

    struct stat sb;
    CHECK_RET(fstat(fd, &sb), 0, "fstat 初始");
    CHECK(sb.st_size == 0, "新文件 size == 0");

    const char *payload = "0123456789";
    ssize_t w = write(fd, payload, 10);
    CHECK(w == 10, "write 10 bytes");

    CHECK_RET(fstat(fd, &sb), 0, "fstat write 后");
    CHECK(sb.st_size == 10, "write 后 size == 10");

    close(fd);
    unlink(path);
}

/* fstatat(dirfd 为普通 fd, "", AT_EMPTY_PATH) 应等价 fstat */
static void test_fstatat_empty_path_on_regfd(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open REG");

    struct stat a, b;
    CHECK_RET(fstat(fd, &a), 0, "fstat fd");
    CHECK_RET(fstatat(fd, "", &b, AT_EMPTY_PATH), 0,
              "fstatat(fd, \"\", AT_EMPTY_PATH)");
    CHECK(a.st_ino == b.st_ino, "fstat / fstatat(AT_EMPTY_PATH) inode 一致");
    CHECK(a.st_size == b.st_size, "fstat / fstatat(AT_EMPTY_PATH) size 一致");
    close(fd);
}

/* statx mask=0 时 Linux 仍返回基本信息（mask ≥ STATX_TYPE|...) */
static void test_statx_mask_zero(void)
{
    struct statx_buf stx;
    memset(&stx, 0, sizeof(stx));
    CHECK_RET(raw_statx(AT_FDCWD, REG, 0, 0, &stx), 0,
              "statx mask=0 (不请求任何字段)");
    /* 内核会返回它认为基本的字段；至少应填类型 */
    CHECK((stx.stx_mask & STATX_TYPE) != 0,
          "statx mask=0 → stx_mask 仍含 STATX_TYPE");
}

/* 路径包含 .. 应可回到父级 */
static void test_stat_parent_traversal(void)
{
    char path[256];
    snprintf(path, sizeof(path), "%s/subdir/../regfile.txt", BASE);
    struct stat sb;
    CHECK_RET(stat(path, &sb), 0, "stat subdir/../regfile.txt");
    CHECK(S_ISREG(sb.st_mode), "subdir/../regfile.txt → regular");
    CHECK(sb.st_size == 12, "subdir/../regfile.txt: size 12");
}

/* 绝对路径中的重复斜线应被规范化 */
static void test_stat_double_slash(void)
{
    char path[256];
    snprintf(path, sizeof(path), "/tmp//starry_stat_test//regfile.txt");
    struct stat sb;
    CHECK_RET(stat(path, &sb), 0, "stat //tmp//starry_stat_test//regfile.txt");
    CHECK(S_ISREG(sb.st_mode), "双斜线路径 → regular");
}

/* ---------- statfs / fstatfs ---------- */

static void test_statfs_happy(void)
{
    struct statfs sb;
    CHECK_RET(statfs(BASE, &sb), 0, "statfs(BASE)");
    CHECK(sb.f_bsize > 0, "statfs f_bsize > 0");
    CHECK(sb.f_namelen > 0, "statfs f_namelen > 0");
}

static void test_fstatfs_happy(void)
{
    int fd = open(REG, O_RDONLY);
    CHECK(fd >= 0, "open reg");
    struct statfs sb;
    CHECK_RET(fstatfs(fd, &sb), 0, "fstatfs(fd)");
    CHECK(sb.f_bsize > 0, "fstatfs f_bsize > 0");
    close(fd);
}

static void test_statfs_enoent(void)
{
    struct statfs sb;
    CHECK_ERR(statfs("/no/such/starry/path/xyz", &sb), ENOENT,
              "statfs 不存在路径 → ENOENT");
}

static void test_fstatfs_ebadf(void)
{
    struct statfs sb;
    CHECK_ERR(fstatfs(-1, &sb), EBADF, "fstatfs(-1) → EBADF");
}

/* ---------- main ---------- */

int main(void)
{
    TEST_START("stat/lstat/fstat/fstatat/statx/statfs/fstatfs 语义");

    setup();

    /* 正常路径 */
    test_stat_regular_file();
    test_stat_directory();
    test_stat_symlink_follows();
    test_lstat_symlink_no_follow();
    test_fstat_regular_file();
    test_fstat_directory();
    test_stat_lstat_same_for_regular();

    /* 错误路径 */
    test_enoent_missing();
    test_enoent_empty_path();
    test_enoent_dangling_symlink();
    test_eloop();
    test_enotdir();
    test_ebadf_fstat();
    test_ebadf_closed();
    test_efault_statbuf_null();
    test_fstatat_einval_unknown_flag();

    /* fstatat */
    test_fstatat_symlink_nofollow();
    test_fstatat_at_empty_path();
    test_fstatat_empty_path_no_flag_enoent();

    /* statx */
    test_statx_regular();
    test_statx_symlink_nofollow();
    test_statx_empty_path_with_flag();
    test_statx_empty_path_no_flag_enoent();
    test_statx_reserved_mask_einval();
    test_statx_sync_type_einval();

    /* 深层覆盖 */
    test_fstatat_relative_to_dirfd();
    test_fstatat_abs_ignores_dirfd();
    test_fstat_pipe();
    test_stat_chardev();
    test_directory_nlink();
    test_stat_after_ftruncate();
    test_stat_after_chmod();
    test_statx_mask_contains_basic();
    test_statx_chardev_rdev();
    test_statx_unknown_flag_einval();
    test_fstat_huge_fd_ebadf();
    test_fstatat_dirfd_is_file();
    test_stat_fstat_same_dev_ino();
    test_stat_hardlink_nlink();
    test_fstat_size_after_write();
    test_fstatat_empty_path_on_regfd();
    test_statx_mask_zero();
    test_stat_parent_traversal();
    test_stat_double_slash();

    /* statfs / fstatfs */
    test_statfs_happy();
    test_fstatfs_happy();
    test_statfs_enoent();
    test_fstatfs_ebadf();

    teardown();

    TEST_DONE();
}
