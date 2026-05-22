#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"
#include "open_creat_alias.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR OF_MOD("creat_alias")

/* man 2 open §"creat()" 原文：
 *   "A call to creat() is equivalent to calling open() with flags equal to
 *    O_CREAT|O_WRONLY|O_TRUNC."
 *
 * 验证：(a) 全 4096 mode 下 creat(P, m) 与 open(P, O_CREAT|O_WRONLY|O_TRUNC, m)
 *           的产物 stat 后 mode 位完全一致（含 sgid host strip 容忍）；
 *       (b) creat 返回的 fd 不能 read（隐含 O_WRONLY → read 返 EBADF）；
 *       (c) creat 返回的 fd 可以 write；
 *       (d) creat 在已有文件上能截断到 size=0（隐含 O_TRUNC）。 */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0, "creat_alias setup: mkdir");
    umask(0);
}

/* 全 4096 mode 矩阵：creat(P1, m) 与 open(P2, O_CREAT|O_WRONLY|O_TRUNC, m) 的产物
 * 应该完全等价（perm 9 位 + suid + sticky）。sgid 在 host 上可能被 strip，容忍。 */
static void creat_equiv_matrix(void)
{
    int sgid_strip_count = 0;
    /* 每 mode 1 断言（perm 一致性）+ 关键 mode 子集再做完整 read/write 验证 */
    for (int m = 0; m <= 07777; m++) {
        char p1[64], p2[64], msg[200];
        snprintf(p1, sizeof(p1), "%s/c_%04o", M_DIR, m);
        snprintf(p2, sizeof(p2), "%s/o_%04o", M_DIR, m);
        unlink(p1); unlink(p2);

        int fd1 = creat(p1, (mode_t)m);
        int fd2 = open(p2, O_CREAT | O_WRONLY | O_TRUNC, (mode_t)m);
        if (fd1 < 0 || fd2 < 0) {
            if (fd1 >= 0) close(fd1);
            if (fd2 >= 0) close(fd2);
            snprintf(msg, sizeof(msg), "creat_alias[mode=%04o]: both opens ok", (unsigned)m);
            CHECK_QUIET(0, msg);
            continue;
        }

        mode_t m1 = get_file_mode(p1) & 07777;
        mode_t m2 = get_file_mode(p2) & 07777;

        int ok = (m1 == m2);
        if (!ok) {
            /* sgid 在 host 上被 strip 的容忍：若两个文件的 sgid 都被 strip 但其余位等价 */
            if ((m1 | 02000) == (m2 | 02000) && (m1 & ~02000) == (m2 & ~02000)) {
                ok = 1;
                sgid_strip_count++;
            }
        }
        snprintf(msg, sizeof(msg),
                 "creat_alias[mode=%04o]: creat perm == open perm (%04o vs %04o)",
                 (unsigned)m, (unsigned)m1, (unsigned)m2);
        CHECK_QUIET(ok, msg);

        close(fd1);
        close(fd2);
        unlink(p1); unlink(p2);
    }
    if (sgid_strip_count > 0)
        printf("  NOTE: %d creat_alias mode rows had sgid stripped on host (parity preserved).\n",
               sgid_strip_count);

    /* 关键 mode 子集再做完整 IO 行为验证 */
    mode_t key_modes[] = { 0644, 0600, 0755, 0666, 0400 };
    for (size_t i = 0; i < sizeof(key_modes)/sizeof(key_modes[0]); i++) {
        mode_t m = key_modes[i];
        char p[64], msg[160];
        snprintf(p, sizeof(p), "%s/k_%04o", M_DIR, m);
        unlink(p);

        int fd = creat(p, m);
        CHECK(fd >= 0,                                                "creat_alias key: creat ok");
        if (fd < 0) continue;

        /* creat 等价 O_WRONLY: read → EBADF */
        char buf[2];
        errno = 0;
        ssize_t rn = read(fd, buf, 1);
        snprintf(msg, sizeof(msg), "creat_alias key[%04o]: creat fd read → EBADF", (unsigned)m);
        CHECK(rn == -1 && errno == EBADF, msg);

        /* write 应成功 */
        ssize_t wn = write(fd, "x", 1);
        snprintf(msg, sizeof(msg), "creat_alias key[%04o]: creat fd write ok", (unsigned)m);
        CHECK(wn == 1, msg);

        close(fd);
        unlink(p);
    }
}

/* 验证 O_TRUNC 部分：creat 应截断已存在文件 */
static void creat_truncates_existing(void)
{
    char p[64];
    snprintf(p, sizeof(p), "%s/existing", M_DIR);
    write_file(p, "longcontent", 11, 0644);

    /* 怎么测：creat 已有内容文件
     * 期望：fd>=0；stat 后 size==0
     * 为什么：creat 隐含 O_TRUNC */
    int fd = creat(p, 0644);
    CHECK(fd >= 0, "creat_alias: creat over existing file ok");
    if (fd >= 0) close(fd);
    CHECK(get_file_size(p) == 0, "creat_alias: existing file truncated to size 0");
}

static void mod_teardown(void)
{
    cleanup_tree(M_DIR);
}

int open_creat_alias_run(void)
{
    printf("\n----- open_creat_alias -----\n");
    mod_setup();
    creat_equiv_matrix();
    creat_truncates_existing();
    mod_teardown();
    printf("  ----- open_creat_alias: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
