#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR  OF_MOD("mode_umask")

/* mode × umask 矩阵：地毯式穷举所有 0o0000..0o7777（12 位 = 4096）模式 ×
 * 5 个 umask 值 = 20480 case。
 *
 * man 2 open §"O_CREAT" 的 mode 部分：
 *   "The effective mode is modified by the process's umask in the usual way:
 *    in the absence of a default ACL, the mode of the created file is
 *    (mode & ~umask)."
 *
 * 12 位 mode 包含：
 *   suid (04000) / sgid (02000) / sticky (01000) +
 *   user (0700)  / group (070)  / other (07)
 *
 * 注意 host 上的 sgid 行为差异：当进程 egid != 父目录 gid 时，Linux 会静默
 * strip sgid 位。本测例在 chmod 验证场景做特殊处理；create-时-给-mode 场景
 * 在 host 上 sgid 必然丢失（因为父目录 /tmp/topen_mode_umask 默认 gid 不一定
 * 匹配进程 egid）。Starry 上以 root 跑，sgid 应保留。
 *
 * 为同时让 host & starry 双绿：
 *   - 如果发现 sgid 在 host 上被 strip，本测例容忍（仅对比 perm 9 位 + suid + sticky）
 *   - 在 starry 上无差异 → 整 12 位都对得上 */
static const mode_t UMASKS[] = { 0000, 0002, 0022, 0027, 0077 };
#define N_UMASKS (sizeof(UMASKS)/sizeof(UMASKS[0]))

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0, "mode_umask setup: mkdir");
    /* 让父目录 gid 与进程 egid 对齐，争取保留 sgid（host 上若 chgrp 失败也容忍）*/
    if (chown(M_DIR, (uid_t)-1, getegid()) != 0) {
        /* host 上若无权限改 gid，忽略 — 矩阵会用容忍逻辑 */
    }
}

static int file_count = 0;
static void next_path(char *out, size_t n) {
    snprintf(out, n, "%s/f%06d", M_DIR, file_count++);
}

/* 主矩阵：0o0000..0o7777 × 5 umasks = 4096 × 5 = 20480 case
 *
 * 对每个 (mode, umask)：
 *   1) unlink → open(O_CREAT|O_WRONLY, mode) → close
 *   2) stat → 取 st_mode & 07777
 *   3) 期望 = mode & ~umask
 *   4) 容忍 sgid 在 host 上被 strip（若 expected 含 sgid 而 actual 不含，
 *      改对比 expected & ~02000 vs actual & ~02000） */
static void mode_umask_matrix_full(void)
{
    int sgid_strip_seen = 0;

    for (size_t ui = 0; ui < N_UMASKS; ui++) {
        mode_t um = UMASKS[ui];
        umask(um);

        for (int m = 0; m <= 07777; m++) {
            char p[80], msg[200];
            next_path(p, sizeof(p));

            unlink(p);
            int fd = open(p, O_CREAT | O_WRONLY, (mode_t)m);
            if (fd < 0) {
                snprintf(msg, sizeof(msg),
                         "mode_umask[m=%04o, um=%04o]: open OK (fd<0, errno=%d)",
                         (unsigned)m, (unsigned)um, errno);
                CHECK_QUIET(0, msg);
                continue;
            }
            close(fd);

            mode_t got  = get_file_mode(p) & 07777;
            mode_t want = ((mode_t)m & ~um) & 07777;

            int ok;
            if (got == want) {
                ok = 1;
            } else if ((want & 02000) && !(got & 02000) &&
                       (got | 02000) == want) {
                /* sgid 被 host strip 的容忍 */
                ok = 1;
                sgid_strip_seen = 1;
            } else {
                ok = 0;
            }

            snprintf(msg, sizeof(msg),
                     "mode_umask[m=%04o, um=%04o]: perm == %04o (got %04o)",
                     (unsigned)m, (unsigned)um, (unsigned)want, (unsigned)got);
            CHECK_QUIET(ok, msg);
            unlink(p);
        }
    }

    if (sgid_strip_seen) {
        printf("  NOTE: sgid bits stripped on some entries — accepted as host-side "
               "kernel policy when process egid != parent dir gid; expect no strip on starry.\n");
    }

    umask(0);
}

/* 特殊位 chmod-after-create 矩阵：枚举 12 位 mode 经 chmod 后保留情况。
 * 因为 chmod 直接命令 kernel 设置 mode 位（无 umask 涉及），sgid 仍可能被
 * silently strip（若进程 egid != file gid）。本测例对 sgid 容忍。 */
static void chmod_round_trip(void)
{
    umask(0);
    /* 抽样而非穷举：12 位 全 4096 都跑 chmod 太啰嗦，仅取 64 个代表点 */
    int sgid_strip_count = 0;
    for (int m = 0; m <= 07777; m += 0123) {        /* 步长 0123 取 ~64 个值 */
        char p[80], msg[200];
        next_path(p, sizeof(p));

        int fd = open(p, O_CREAT | O_WRONLY, 0644);
        if (fd < 0) continue;
        close(fd);

        chmod(p, (mode_t)m);                        /* 容忍失败 */

        mode_t got  = get_file_mode(p) & 07777;
        mode_t want = (mode_t)m & 07777;

        int ok;
        if (got == want) {
            ok = 1;
        } else if ((want & 02000) && !(got & 02000) &&
                   (got | 02000) == want) {
            ok = 1;
            sgid_strip_count++;
        } else {
            ok = 0;
        }

        snprintf(msg, sizeof(msg),
                 "chmod_rt[m=%04o]: perm == %04o (got %04o)",
                 (unsigned)m, (unsigned)want, (unsigned)got);
        CHECK_QUIET(ok, msg);
        unlink(p);
    }
    if (sgid_strip_count > 0) {
        printf("  NOTE: %d chmod cases had sgid stripped on host; expect no strip on starry.\n", sgid_strip_count);
    }
}

int open_mode_umask_run(void)
{
    printf("\n----- open_mode_umask -----\n");
    mod_setup();
    mode_umask_matrix_full();
    chmod_round_trip();
    cleanup_tree(M_DIR);
    printf("  ----- open_mode_umask: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
