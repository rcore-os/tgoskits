/* procfs_visibility.c — Linux 隐含规范:
 * cred 改变后 /proc/self/status 应同步反映 r/e/s/fsid.
 *
 * man proc(5) §/proc/[pid]/status:
 *   "Uid, Gid: Real, effective, saved set, and filesystem UIDs (GIDs)."
 *   格式: "Uid:\t<real>\t<effective>\t<saved>\t<fs>"
 *   格式: "Gid:\t<real>\t<effective>\t<saved>\t<fs>"
 *
 * starry 实现 procfs 时必须使 sys_setuid/setgid 写入 cred 后,
 * 后续读 /proc/self/status 立刻反映新值 (不能 cache 旧值).
 *
 * 5 维度覆盖:
 *   (a) baseline: root 启动时 Uid: 0 0 0 0
 *   (b) setuid root → 0; 验 Uid 全 0
 *   (c) setuid 1000 root (drops to 1000); 验 Uid: 1000 1000 1000 1000
 *   (d) setgid root → 1000; 验 Gid: 1000 1000 1000 1000 (uid 不变)
 *   (e) setresuid(1000, 2000, 3000); 验 Uid: 1000 2000 3000 2000 (fs == e)
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely_pv(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* 解析 /proc/self/status 中 "Uid:" 或 "Gid:" 行 → 四个 ID. 返 -1 错 */
static int parse_proc_id_line(const char *prefix,
                               uint32_t *r, uint32_t *e, uint32_t *s, uint32_t *fs)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    char line[256];
    int found = -1;
    while (fgets(line, sizeof line, f)) {
        if (strncmp(line, prefix, strlen(prefix)) == 0) {
            if (sscanf(line + strlen(prefix), "%u %u %u %u", r, e, s, fs) == 4) {
                found = 0;
            }
            break;
        }
    }
    fclose(f);
    return found;
}

/* (a) baseline: root 启动时 r=e=s=fs=0 */
static void procfs_uid_baseline_root(void)
{
    /* 测什么: man proc(5) §/proc/[pid]/status Uid line 显示 cred 4 字段.
     * 怎么测: 启动时 (无 setuid) 读 /proc/self/status Uid 行.
     * 期望: 4 字段都 == getuid() 实际值.
     * 为什么: 验证 starry procfs 能正确读 cred 内部 4 字段. */
    if (getuid() != 0) {
        printf("  procfs (a) skip: needs root for baseline check\n");
        return;
    }
    uint32_t r, e, s, fs;
    int rc = parse_proc_id_line("Uid:", &r, &e, &s, &fs);
    CHECK(rc == 0,                                                 "procfs (a) parse /proc/self/status Uid line ok");
    if (rc == 0) {
        CHECK(r == 0 && e == 0 && s == 0 && fs == 0,
              "procfs (a) baseline root: Uid line shows 0 0 0 0");
    }
}

/* (b) setuid(0) keep root → Uid 全 0 */
static void procfs_uid_after_setuid_root(void)
{
    /* 测什么: setuid(0) 后 /proc/self/status Uid 仍 0 (未变).
     * 怎么测: fork → child setuid(0) → 读 Uid 行验.
     * 期望: r=e=s=fs=0.
     * 为什么: 验证 procfs sync — 即使 setuid 走通也不污染. */
    if (getuid() != 0) { printf("  procfs (b) skip: needs root\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setuid(0) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 0 || e != 0 || s != 0 || fs != 0) _exit(1);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (b) setuid(0) keep root → Uid: 0 0 0 0");
}

/* (c) setuid(1000) drops root → Uid: 1000 1000 1000 1000 */
static void procfs_uid_after_setuid_drop(void)
{
    /* 测什么: root setuid(1000) 后 procfs Uid 同步显示 1000 (全 4 字段).
     *         也验 fsuid 跟 euid (= 1000), 不是停在 0.
     * 怎么测: fork → child setuid(1000) → 读 Uid 行.
     * 期望: r=e=s=fs=1000.
     * 为什么: 验证 1) starry has_cap setuid 全设 r/e/s 2) fsuid 跟 euid
     *         3) procfs 立刻反映 (无 stale cache). */
    if (getuid() != 0) { printf("  procfs (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setuid(1000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 1000 || s != 1000 || fs != 1000) {
            printf("  got Uid: %u %u %u %u (expected 1000 1000 1000 1000)\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (c) setuid(1000) → Uid: 1000 1000 1000 1000 (fsuid 跟 euid)");
}

/* (d) setgid(1000) → Gid: 1000 1000 1000 1000, Uid 不变 */
static void procfs_gid_after_setgid(void)
{
    /* 测什么: setgid(1000) 同步反映在 Gid 行, Uid 不动 (验 setgid 不串扰 uid).
     * 怎么测: fork → child setgid(1000) → 读 Gid 行 + Uid 行.
     * 期望: Gid: 1000 1000 1000 1000; Uid 行不变 (root cred).
     * 为什么: 验 starry setgid 独立设 gid + procfs 同步. */
    if (getuid() != 0) { printf("  procfs (d) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setgid(1000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Gid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 1000 || s != 1000 || fs != 1000) _exit(1);
        /* 副带: Uid 应仍 0 */
        uint32_t ur, ue, us, ufs;
        if (parse_proc_id_line("Uid:", &ur, &ue, &us, &ufs) != 0) _exit(97);
        if (ur != 0 || ue != 0 || us != 0 || ufs != 0) _exit(2);
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (d) setgid(1000) → Gid: 1000×4, Uid 不变 (root)");
}

/* (e) setresuid(1000,2000,3000) → Uid: 1000 2000 3000 2000 */
static void procfs_uid_after_setresuid(void)
{
    /* 测什么: 三参数 setresuid 时 Uid 行 r/e/s/fs 分别独立显示.
     *         fs 应 == e (Linux 总同步 fsuid = euid).
     * 怎么测: fork → child setresuid(1000,2000,3000) → 读 Uid 行.
     * 期望: 1000 2000 3000 2000.
     * 为什么: 验 starry sys_setresuid 三参数独立写 + fsuid 跟 euid + procfs 同步. */
    if (getuid() != 0) { printf("  procfs (e) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 2000, 3000) != 0) _exit(99);
        uint32_t r, e, s, fs;
        if (parse_proc_id_line("Uid:", &r, &e, &s, &fs) != 0) _exit(98);
        if (r != 1000 || e != 2000 || s != 3000 || fs != 2000) {
            printf("  got Uid: %u %u %u %u (expected 1000 2000 3000 2000)\n", r, e, s, fs);
            _exit(1);
        }
        _exit(0);
    }
    int status;
    waitpid_safely_pv(pid, &status);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "procfs (e) setresuid(1k,2k,3k) → Uid: 1000 2000 3000 2000 (fs跟e)");
}

int procfs_visibility_run(void)
{
    printf("\n----- procfs_visibility -----\n");
    procfs_uid_baseline_root();
    procfs_uid_after_setuid_root();
    procfs_uid_after_setuid_drop();
    procfs_gid_after_setgid();
    procfs_uid_after_setresuid();
    printf("  ----- procfs_visibility: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
