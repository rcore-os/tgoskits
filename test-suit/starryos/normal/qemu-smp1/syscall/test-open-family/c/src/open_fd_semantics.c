#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define M_DIR  OF_MOD("fd_sem")
#define M_FILE M_DIR "/file"

/* fd 分配 + 独立 file description 语义 + RLIMIT_NOFILE 触达 EMFILE。
 *
 * man 2 open 原文（DESCRIPTION 段顶部）：
 *   "The return value of open() is a file descriptor, a small, nonnegative
 *    integer ... A successful call will return the lowest-numbered file
 *    descriptor not currently open for the process."
 *
 *   "By default, the new file descriptor is set to remain open across an
 *    execve(2) ... The file offset is set to the beginning of the file
 *    (see lseek(2))."
 *
 *   "Each open() of the file creates a new open file description; thus,
 *    there may be multiple open file descriptions corresponding to a file
 *    inode."
 *
 *   "A file descriptor is a reference to an open file description; this
 *    reference is unaffected if pathname is subsequently removed or
 *    modified to refer to a different file."
 *
 * man 2 open §"EMFILE"：
 *   "EMFILE — The per-process limit on the number of open file descriptors
 *    has been reached (see the description of RLIMIT_NOFILE in
 *    getrlimit(2))."
 *
 * 8 子函数：lowest-fd / 独立 OFD / dup 共享 / 初始 offset / unlink-survive /
 *          dup3+CLOEXEC / 64-fd 并发 / RLIMIT_NOFILE → EMFILE. */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "fd_sem setup: mkdir");
    CHECK(write_file(M_FILE, "0123456789", 10, 0644) == 0,        "fd_sem setup: 10-byte file");
}

static void fd_lowest_available(void)
{
    int fd1 = open(M_FILE, O_RDONLY);
    int fd2 = open(M_FILE, O_RDONLY);
    int fd3 = open(M_FILE, O_RDONLY);
    CHECK(fd1 >= 0 && fd2 >= 0 && fd3 >= 0,                       "fd_lowest: 3 opens ok");
    CHECK(fd2 == fd1 + 1,                                         "fd_lowest: fd2 == fd1+1");
    CHECK(fd3 == fd2 + 1,                                         "fd_lowest: fd3 == fd2+1");
    close(fd2);
    int fd4 = open(M_FILE, O_RDONLY);
    CHECK(fd4 == fd2,                                             "fd_lowest: new fd reuses fd2's slot");
    close(fd1); close(fd3); close(fd4);
}

static void fd_independent_offsets(void)
{
    int fd1 = open(M_FILE, O_RDONLY);
    int fd2 = open(M_FILE, O_RDONLY);
    CHECK(fd1 >= 0 && fd2 >= 0,                                   "fd_indep: two opens ok");

    char b1[2] = {0}, b2[2] = {0};
    lseek(fd1, 0, SEEK_SET);
    lseek(fd2, 5, SEEK_SET);
    read(fd1, b1, 1);
    read(fd2, b2, 1);
    CHECK(b1[0] == '0',                                           "fd_indep: fd1 reads at offset 0 -> '0'");
    CHECK(b2[0] == '5',                                           "fd_indep: fd2 reads at offset 5 -> '5'");
    close(fd1); close(fd2);
}

static void fd_dup_shares_offset(void)
{
    int fd1 = open(M_FILE, O_RDONLY);
    int fd2 = dup(fd1);
    CHECK(fd1 >= 0 && fd2 >= 0,                                   "fd_dup: open + dup ok");

    char b1[2] = {0}, b2[2] = {0};
    lseek(fd1, 3, SEEK_SET);
    read(fd2, b2, 1);
    read(fd1, b1, 1);
    CHECK(b2[0] == '3',                                           "fd_dup: fd2 read uses shared offset 3 -> '3'");
    CHECK(b1[0] == '4',                                           "fd_dup: fd1 read after fd2 advanced -> '4'");
    close(fd1); close(fd2);
}

static void fd_initial_offset_zero(void)
{
    int fd = open(M_FILE, O_RDONLY);
    CHECK(fd >= 0,                                                "fd_init_off: open ok");
    if (fd < 0) return;
    off_t off = lseek(fd, 0, SEEK_CUR);
    CHECK(off == 0,                                               "fd_init_off: initial offset == 0");
    close(fd);
}

static void fd_survives_unlink(void)
{
    char p[64];
    snprintf(p, sizeof(p), "%s/transient", M_DIR);
    write_file(p, "abc", 3, 0644);

    int fd = open(p, O_RDONLY);
    CHECK(fd >= 0,                                                "fd_unlink: open ok");
    if (fd < 0) return;
    CHECK(unlink(p) == 0,                                         "fd_unlink: unlink ok");
    char buf[8] = {0};
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    CHECK(n == 3 && memcmp(buf, "abc", 3) == 0,                   "fd_unlink: read after unlink ok");
    close(fd);
}

static void fd_dup3_independent_cloexec(void)
{
    int fd1 = open(M_FILE, O_RDONLY);
    CHECK(fd1 >= 0,                                               "fd_dup3: source open ok");
    if (fd1 < 0) return;

    int target = fd1 + 10;
    int fd2 = dup3(fd1, target, O_CLOEXEC);
    CHECK(fd2 == target,                                          "fd_dup3: dup3 returns target fd");
    if (fd2 >= 0) {
        int fl = fcntl(fd2, F_GETFD);
        CHECK(fl >= 0 && (fl & FD_CLOEXEC) != 0,                  "fd_dup3: target has FD_CLOEXEC");
        close(fd2);
    }
    close(fd1);
}

/* 同时持有大量 fd（验证 fd 表能扩展）*/
static void fd_many_open_simultaneous(void)
{
    enum { N_FDS = 64 };
    int fds[N_FDS];
    int opened = 0;
    for (int i = 0; i < N_FDS; i++) {
        fds[i] = open(M_FILE, O_RDONLY);
        if (fds[i] < 0) break;
        opened++;
    }
    CHECK(opened == N_FDS,                                        "fd_many: 64 simultaneous opens ok");
    /* 所有 fd 应可读 */
    int all_read_ok = 1;
    for (int i = 0; i < opened; i++) {
        char b[1] = {0};
        if (read(fds[i], b, 1) != 1 || b[0] != '0') { all_read_ok = 0; break; }
    }
    CHECK(all_read_ok,                                            "fd_many: all 64 fds readable independently");
    for (int i = 0; i < opened; i++) close(fds[i]);
}

/* RLIMIT_NOFILE 设小，验证 EMFILE
 *
 * 注：starry kernel `add_file_like` 检查 RLIMIT_NOFILE 直接返 TooManyOpenFiles → EMFILE。
 * 本测例触发并验证。 */
static void fd_rlimit_nofile_emfile(void)
{
    struct rlimit rl_old;
    if (getrlimit(RLIMIT_NOFILE, &rl_old) != 0) {
        CHECK(0, "fd_rlimit: getrlimit failed");
        return;
    }

    /* 设 limit = 当前 + 0 → 任何新 open 都应失败（其实 setrlimit 设 rl_cur）
     * 实际方案：先关闭所有可释放 fd，留 stdin/stdout/stderr，再设 cur=8 */
    struct rlimit rl_new = { .rlim_cur = 16, .rlim_max = rl_old.rlim_max };
    if (setrlimit(RLIMIT_NOFILE, &rl_new) != 0) {
        CHECK(0, "fd_rlimit: setrlimit failed");
        return;
    }

    /* 打到达上限，记录第一次 EMFILE */
    int fds[20];
    int last_ok_idx = -1;
    int emfile_seen = 0;
    for (int i = 0; i < 20; i++) {
        errno = 0;
        fds[i] = open(M_FILE, O_RDONLY);
        if (fds[i] >= 0) last_ok_idx = i;
        else if (errno == EMFILE) { emfile_seen = 1; break; }
        else                      break;
    }
    CHECK(emfile_seen,                                            "fd_rlimit: EMFILE eventually seen");
    CHECK(last_ok_idx >= 0,                                       "fd_rlimit: at least 1 open succeeded before EMFILE");

    for (int i = 0; i <= last_ok_idx; i++)
        if (fds[i] >= 0) close(fds[i]);

    /* 还原 rlimit */
    setrlimit(RLIMIT_NOFILE, &rl_old);
}

int open_fd_semantics_run(void)
{
    printf("\n----- open_fd_semantics -----\n");
    mod_setup();
    fd_lowest_available();
    fd_independent_offsets();
    fd_dup_shares_offset();
    fd_initial_offset_zero();
    fd_survives_unlink();
    fd_dup3_independent_cloexec();
    fd_many_open_simultaneous();
    fd_rlimit_nofile_emfile();
    cleanup_tree(M_DIR);
    printf("  ----- open_fd_semantics: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
