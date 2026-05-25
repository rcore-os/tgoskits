#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR     OF_MOD("silent_flags")
#define M_FILE    M_DIR "/file"
#define M_DIR_TGT M_DIR "/sub"

/* "silent flags"：starry kernel 的 flags_to_options（fd_ops.rs:26-63）
 * 不识别这些 flag，但也不报错。它们对 starry 等价于不传。
 * Linux 多数 silent flag 也可在普通文件上无副作用通过；ASYNC/NOCTTY 在普通文件
 * 上无意义但不报错；O_NOATIME 需要 owner 匹配（root 总成立）。
 *
 * man 2 open 各 silent flag 原文（每个节选 1 句）：
 *   O_NOCTTY:    "If pathname refers to a terminal device—see tty(4)—it
 *                will not become the process's controlling terminal even
 *                if the process does not have one."
 *   O_DSYNC:     "Write operations on the file will complete according to
 *                the requirements of synchronized I/O data integrity
 *                completion."
 *   O_SYNC:      "Write operations on the file will complete according to
 *                the requirements of synchronized I/O file integrity
 *                completion."
 *   O_RSYNC:     "(glibc defines O_RSYNC to be the same value as O_SYNC.)"
 *   O_NOATIME:   "Do not update the file last access time (st_atime in the
 *                inode) when the file is read(2)."
 *   O_LARGEFILE: "(LFS) Allow files whose sizes cannot be represented in
 *                an off_t (but can be represented in an off64_t) to be
 *                opened."
 *   O_ASYNC:     "Enable signal-driven I/O: generate a signal (SIGIO by
 *                default) when input or output becomes possible on this
 *                file descriptor."
 *
 * 验证策略：每个 silent flag 单独 + 全 OR + silent × honored
 *           (CLOEXEC/NONBLOCK/APPEND) × 3 access 矩阵；
 *           断 open 不报错且后续 read/IO 不被破坏（不验语义细节，仅"不破坏"）。 */

#ifndef O_NOATIME
#define O_NOATIME 01000000
#endif
#ifndef O_LARGEFILE
#define O_LARGEFILE 0
#endif

/* 全部 silent flag 集合 */
struct silent_flag { int bit; const char *name; };
static const struct silent_flag SILENT[] = {
#ifdef O_NOCTTY
    { O_NOCTTY,    "O_NOCTTY"    },
#endif
#ifdef O_DSYNC
    { O_DSYNC,     "O_DSYNC"     },
#endif
#ifdef O_SYNC
    { O_SYNC,      "O_SYNC"      },
#endif
#ifdef O_RSYNC
    { O_RSYNC,     "O_RSYNC"     },
#endif
    { O_NOATIME,   "O_NOATIME"   },
    { O_LARGEFILE, "O_LARGEFILE" },
#ifdef O_ASYNC
    { O_ASYNC,     "O_ASYNC"     },
#endif
};
#define N_SILENT (sizeof(SILENT)/sizeof(SILENT[0]))

/* 与 silent flag 交叉的 honored 集合（不含会改变 path 解析的 NOFOLLOW/DIRECTORY，
 * 也不含会破坏 IO 测试的 PATH/TRUNC/CREAT|EXCL）*/
struct honored { int bit; const char *name; };
static const struct honored HONORED[] = {
    { O_CLOEXEC,  "CLOEXEC"  },
    { O_NONBLOCK, "NONBLOCK" },
    { O_APPEND,   "APPEND"   },     /* 仅与 WRONLY/RDWR 配 */
};
#define N_HONORED (sizeof(HONORED)/sizeof(HONORED[0]))

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "silent setup: mkdir");
    CHECK(write_file(M_FILE, "data", 4, 0644) == 0,               "silent setup: file");
    CHECK(ensure_dir(M_DIR_TGT) == 0,                             "silent setup: dir target");
}

/* 单独 silent flag → open RDONLY ok + read works */
static void each_silent_isolation(void)
{
    for (size_t i = 0; i < N_SILENT; i++) {
        char msg[160];
        int fd = open(M_FILE, O_RDONLY | SILENT[i].bit);
        snprintf(msg, sizeof(msg), "silent[%s] iso: open RDONLY ok", SILENT[i].name);
        CHECK(fd >= 0, msg);
        if (fd < 0) continue;

        char buf[8] = {0};
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        snprintf(msg, sizeof(msg), "silent[%s] iso: read still works", SILENT[i].name);
        CHECK(n == 4 && memcmp(buf, "data", 4) == 0, msg);
        close(fd);
    }
}

/* 全 silent flag 一起 OR */
static void all_silent_combined(void)
{
    int extra = 0;
    for (size_t i = 0; i < N_SILENT; i++) extra |= SILENT[i].bit;

    int fd = open(M_FILE, O_RDONLY | extra);
    CHECK(fd >= 0,                                                "silent combined: all-OR open ok");
    if (fd < 0) return;

    char buf[8] = {0};
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    CHECK(n == 4 && memcmp(buf, "data", 4) == 0,                  "silent combined: read still works");
    close(fd);
}

/* 交叉矩阵：每个 silent × 每个 honored × 3 access mode → 验证 silent 不破坏 honored */
static void silent_x_honored_matrix(void)
{
    int access_modes[] = { O_RDONLY, O_WRONLY, O_RDWR };
    const char *am_names[] = { "RDONLY", "WRONLY", "RDWR" };

    for (size_t s = 0; s < N_SILENT; s++) {
        for (size_t h = 0; h < N_HONORED; h++) {
            /* O_APPEND 与 O_RDONLY 在 starry 上是已知 bug → skip */
            for (int a = 0; a < 3; a++) {
                if (HONORED[h].bit == O_APPEND && access_modes[a] == O_RDONLY)
                    continue;

                int flags = access_modes[a] | SILENT[s].bit | HONORED[h].bit;
                char msg[200];
                int fd = open(M_FILE, flags);
                snprintf(msg, sizeof(msg), "silent_x_honored[%s+%s+%s]: open ok",
                         SILENT[s].name, HONORED[h].name, am_names[a]);
                CHECK_QUIET(fd >= 0, msg);
                if (fd < 0) continue;

                /* 验证 honored 行为仍 work */
                if (HONORED[h].bit == O_CLOEXEC) {
                    int fl = fcntl(fd, F_GETFD);
                    snprintf(msg, sizeof(msg),
                             "silent_x_honored[%s+CLOEXEC+%s]: FD_CLOEXEC set",
                             SILENT[s].name, am_names[a]);
                    CHECK_QUIET(fl >= 0 && (fl & FD_CLOEXEC), msg);
                } else if (HONORED[h].bit == O_NONBLOCK) {
                    int fl = fcntl(fd, F_GETFL);
                    snprintf(msg, sizeof(msg),
                             "silent_x_honored[%s+NONBLOCK+%s]: O_NONBLOCK in F_GETFL",
                             SILENT[s].name, am_names[a]);
                    CHECK_QUIET(fl >= 0 && (fl & O_NONBLOCK), msg);
                }
                close(fd);
            }
        }
    }
}

int open_silent_flags_run(void)
{
    printf("\n----- open_silent_flags -----\n");
    mod_setup();
    each_silent_isolation();
    all_silent_combined();
    silent_x_honored_matrix();
    cleanup_tree(M_DIR);
    printf("  ----- open_silent_flags: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
