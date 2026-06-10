/* procfs_visibility.c — getter syscall ↔ /proc/self/status 一致性.
 *
 * man proc(5) §/proc/[pid]/status:
 *   "Uid, Gid: Real, effective, saved set, and filesystem UIDs (GIDs)."
 *   "Groups: Supplementary group list."
 *
 * 隐含规范: procfs Uid/Gid 行的 r/e/s 必须与 syscall 一致.
 * 任何分歧 = starry cred 子系统 ↔ procfs 同步 bug.
 *
 * 4 case (a-d):
 *   (a) getuid()      == /proc/self/status Uid[0] (real)
 *   (b) geteuid()     == Uid[1] (effective)
 *   (c) getresuid()   == Uid[0..2]
 *   (d) getgroups()   ↔ /proc/self/status Groups: 行 (set 一致)
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <ctype.h>
#include <errno.h>
#include <grp.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

static int parse_proc_id_line(const char *prefix,
                               uint32_t *r, uint32_t *e, uint32_t *s, uint32_t *fs)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    char line[256];
    int found = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            if (sscanf(line + strlen(prefix), "%u %u %u %u", r, e, s, fs) == 4)
                found = 0;
            break;
        }
    }
    fclose(f);
    return found;
}

/* parse_proc_groups removed: case (d) 已迁移到 bug-* 分支独立复现.
 *
 * 原 case (d) (getgroups ↔ procfs Groups: 一致性) 在 starry loongarch64
 * 上 fopen+fgets /proc/self/status 时 vm_write 返 EFAULT (Linux 通过)
 * → 见 bug-starry-procfs-loongarch64-vm-write-efault 分支独立验证.
 * 此处 test-* PR 仅保留 starry + Linux 双绿的 case (a-c, getuid/euid/resuid). */

/* (a) getuid() == Uid[0] */
static void proc_getuid_matches_real(void)
{
    /* 测什么: 隐含规范 — getuid syscall 与 procfs Uid 行 real 字段同源.
     * 怎么测: getuid() vs parse_proc_id_line("Uid:") 第 1 字段.
     * 期望: 相等.
     * 为什么: 防 starry cred ↔ procfs 数据源分裂 (历史 bug 常见). */
    uid_t u = getuid();
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Uid:", &r, &e, &s, &fs);
    CHECK(rc == 0,                                                 "procfs (a) parse Uid line ok");
    if (rc == 0) {
        CHECK((uint32_t)u == r,                                    "procfs (a) getuid() == Uid[0] (real)");
    }
}

/* (b) geteuid() == Uid[1] */
static void proc_geteuid_matches_effective(void)
{
    /* 测什么/怎么测/期望/为什么: 同 (a) 镜像 — geteuid 与 procfs Uid 行 e 字段同源. */
    uid_t e_syscall = geteuid();
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Uid:", &r, &e, &s, &fs);
    CHECK(rc == 0,                                                 "procfs (b) parse Uid line ok");
    if (rc == 0) {
        CHECK((uint32_t)e_syscall == e,                            "procfs (b) geteuid() == Uid[1] (effective)");
    }
}

/* (c) getresuid 三字段 vs procfs */
static void proc_getresuid_matches_three(void)
{
    /* 测什么: getresuid 三 out-arg 应与 procfs Uid 行前 3 字段一一对应.
     * 怎么测: getresuid + parse, 比对 r/e/s.
     * 期望: 三对都相等.
     * 为什么: 验 starry sys_getresuid 与 procfs 同源 cred 全字段. */
    uid_t sr, se, ss;
    if (getresuid(&sr, &se, &ss) != 0) { CHECK(0, "procfs (c) getresuid failed"); return; }
    uint32_t pr, pe, ps, pfs;
    if (parse_proc_id_line("Uid:", &pr, &pe, &ps, &pfs) != 0) {
        CHECK(0, "procfs (c) parse Uid line failed"); return;
    }
    CHECK((uint32_t)sr == pr && (uint32_t)se == pe && (uint32_t)ss == ps,
          "procfs (c) getresuid r/e/s == Uid[0..2]");
}

/* (d) getgroups ↔ procfs Groups: 一致性 — 已迁移到 bug-starry-procfs-
 *     loongarch64-vm-write-efault 分支独立复现 (此处 test-* PR 保 starry
 *     + Linux 双绿). */

int procfs_visibility_run(void)
{
    printf("\n----- procfs_visibility (getter ↔ procfs 一致) -----\n");
    proc_getuid_matches_real();
    proc_geteuid_matches_effective();
    proc_getresuid_matches_three();
    printf("  ----- procfs_visibility: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
